#![cfg(feature = "firebird")]
// Modified for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.

use std::sync::Arc;

use quaint::prelude::{ConnectionInfo, ExternalConnectionInfo, SqlFamily};
use query_core::QueryDocument;
use request_handlers::{JsonBody, JsonSingleQuery, RequestBody};
use serde_json::{Value, json};

fn query_schema() -> schema::QuerySchema {
    // Validate the actual connector, including its native types and capability
    // gates. A PostgreSQL fixture with a substituted SQL visitor would miss them.
    let schema = psl::validate_without_extensions(include_str!("data/firebird.prisma").into());
    assert!(!schema.diagnostics.has_errors(), "{:?}", schema.diagnostics);
    schema::build(Arc::new(schema), true)
}

fn compile(action: &str, arguments: Value, selection: Value) -> Value {
    compile_model("Item", action, arguments, selection)
}

fn compile_model(model: &str, action: &str, arguments: Value, selection: Value) -> Value {
    try_compile_model(model, action, arguments, selection).unwrap()
}

fn try_compile_model(
    model: &str,
    action: &str,
    arguments: Value,
    selection: Value,
) -> Result<Value, query_compiler::CompileError> {
    try_compile_with_schema(
        Arc::new(query_schema()),
        SqlFamily::Firebird,
        model,
        action,
        arguments,
        selection,
    )
}

fn try_compile_with_schema(
    schema: Arc<schema::QuerySchema>,
    family: SqlFamily,
    model: &str,
    action: &str,
    arguments: Value,
    selection: Value,
) -> Result<Value, query_compiler::CompileError> {
    let query: JsonSingleQuery = serde_json::from_value(json!({
        "modelName": model,
        "action": action,
        "query": { "arguments": arguments, "selection": selection },
    }))
    .unwrap();
    let QueryDocument::Single(operation) = RequestBody::Json(JsonBody::Single(query)).into_doc(&schema).unwrap() else {
        panic!("expected one query");
    };
    let connection_info = ConnectionInfo::External(ExternalConnectionInfo::new(family, None, Some(1000), false));
    let plan = query_compiler::compile(&schema, operation, &connection_info)?;
    Ok(serde_json::to_value(plan).unwrap())
}

fn statements(plan: &Value) -> Vec<&Value> {
    fn collect<'a>(node: &'a Value, result: &mut Vec<&'a Value>) {
        match node {
            Value::Object(object) if object.get("type").and_then(Value::as_str) == Some("templateSql") => {
                result.push(node);
            }
            Value::Object(object) => object.values().for_each(|value| collect(value, result)),
            Value::Array(array) => array.iter().for_each(|value| collect(value, result)),
            _ => {}
        }
    }
    let mut result = Vec::new();
    collect(plan, &mut result);
    for statement in &result {
        assert_eq!(
            statement["placeholderFormat"],
            json!({ "prefix": "?", "hasNumbering": false })
        );
    }
    result
}

fn sql(statement: &Value) -> String {
    statement["fragments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|fragment| match fragment["type"].as_str().unwrap() {
            "stringChunk" => fragment["chunk"].as_str().unwrap(),
            "parameter" | "parameterTuple" | "parameterTupleList" => "?",
            other => panic!("unexpected SQL fragment {other}"),
        })
        .collect()
}

fn expressions<'a>(plan: &'a Value, kind: &str) -> Vec<&'a Value> {
    fn collect<'a>(node: &'a Value, kind: &str, result: &mut Vec<&'a Value>) {
        match node {
            Value::Object(object) => {
                if object.get("type").and_then(Value::as_str) == Some(kind) {
                    result.push(node);
                }
                object.values().for_each(|value| collect(value, kind, result));
            }
            Value::Array(array) => array.iter().for_each(|value| collect(value, kind, result)),
            _ => {}
        }
    }
    let mut result = Vec::new();
    collect(plan, kind, &mut result);
    result
}

fn update_input_fields(schema: &schema::QuerySchema, model: &str, action: &str) -> Vec<(String, Vec<String>)> {
    let mutation = schema.find_mutation_field(&format!("{action}{model}")).unwrap();
    let data = mutation
        .arguments()
        .iter()
        .find(|argument| argument.name == "data")
        .unwrap();
    data.field_types()
        .iter()
        .filter_map(schema::InputType::as_object)
        .map(|object| {
            (
                object.identifier.name(),
                object.get_fields().iter().map(|field| field.name.to_string()).collect(),
            )
        })
        .collect()
}

#[track_caller]
fn assert_returned_identifier(plan: &Value, table: &str, columns: &[&str]) {
    assert_eq!(expressions(plan, "transaction").len(), 1, "{plan:#}");
    assert!(expressions(plan, "mapRecord").is_empty(), "{plan:#}");
    let updates: Vec<_> = statements(plan)
        .into_iter()
        .map(sql)
        .filter(|query| query.starts_with(&format!("UPDATE \"{table}\"")))
        .collect();
    assert_eq!(updates.len(), 1, "{plan:#}");
    let columns = columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    assert!(updates[0].ends_with(&format!(" RETURNING {columns}")), "{plan:#}");
    assert!(
        expressions(plan, "unique").iter().any(|node| {
            node["args"]["type"] == "query"
                && node["args"]["args"]["type"] == "templateSql"
                && sql(&node["args"]["args"]).starts_with(&format!("UPDATE \"{table}\""))
        }),
        "{plan:#}"
    );
}

#[test]
fn checked_and_unchecked_update_inputs_expose_manual_and_compound_ids() {
    let schema = query_schema();
    for (model, ids) in [("MutableCode", vec!["code"]), ("CompoundItem", vec!["tenant", "id"])] {
        for action in ["updateOne", "updateMany"] {
            let inputs = update_input_fields(&schema, model, action);
            assert_eq!(inputs.len(), 2, "{model}/{action}: {inputs:?}");
            for (name, fields) in inputs {
                for id in &ids {
                    assert!(fields.iter().any(|field| field == id), "{name} omits {id}: {fields:?}");
                }
            }
        }
    }

    let inputs = update_input_fields(&schema, "AutoItem", "updateOne");
    for (name, fields) in inputs {
        let is_unchecked = name.contains("Unchecked");
        assert_eq!(
            fields.iter().any(|field| field == "id"),
            is_unchecked,
            "{name}: {fields:?}"
        );
    }
}

#[test]
fn string_primary_key_update_returns_the_new_key_before_the_transactional_result_read() {
    let plan = compile_model(
        "MutableCode",
        "updateOne",
        json!({ "where": { "code": "UN" }, "data": { "code": { "set": "CX" } } }),
        json!({ "code": true, "description": true }),
    );
    assert_returned_identifier(&plan, "FB_MUTABLE_CODE", &["CODIGO"]);
    let statements = statements(&plan);
    assert_eq!(statements.len(), 3, "{plan:#}");
    let queries: Vec<_> = statements.iter().map(|statement| sql(statement)).collect();
    assert_eq!(queries.iter().filter(|query| query.starts_with("SELECT ")).count(), 2);
    let update = queries.iter().find(|query| query.starts_with("UPDATE ")).unwrap();
    assert!(
        update.starts_with("UPDATE \"FB_MUTABLE_CODE\" SET \"CODIGO\" = ?"),
        "{update}"
    );
    assert!(
        queries
            .iter()
            .all(|query| !query.contains("'UN'") && !query.contains("'CX'"))
    );
}

#[test]
fn integer_primary_key_set_and_atomic_operations_use_the_database_identifier() {
    for input in [
        json!(42),
        json!({ "set": 42 }),
        json!({ "increment": 2 }),
        json!({ "decrement": 2 }),
        json!({ "multiply": 2 }),
        json!({ "divide": 2 }),
    ] {
        let plan = compile(
            "updateOne",
            json!({ "where": { "id": 41 }, "data": { "id": input } }),
            json!({ "id": true, "code": true }),
        );
        assert_returned_identifier(&plan, "FB_ITEM", &["ID"]);
        assert_eq!(statements(&plan).len(), 3, "{plan:#}");
    }
}

#[test]
fn bigint_primary_key_update_preserves_values_beyond_javascript_safe_integer_range() {
    let plan = compile_model(
        "MutableBigInt",
        "updateOne",
        json!({
            "where": { "id": { "$type": "BigInt", "value": "9007199254740993" } },
            "data": { "id": { "set": { "$type": "BigInt", "value": "9007199254740995" } } },
        }),
        json!({ "id": true, "value": true }),
    );
    assert_returned_identifier(&plan, "FB_MUTABLE_BIGINT", &["ID"]);
    assert!(
        statements(&plan)
            .iter()
            .any(|statement| statement["args"].to_string().contains("9007199254740995")),
        "{plan:#}"
    );
}

#[test]
fn compound_primary_key_updates_return_all_components_and_keep_scalar_predicates() {
    for data in [json!({ "id": 42 }), json!({ "tenant": 2, "id": { "increment": 3 } })] {
        let plan = compile_model(
            "CompoundItem",
            "updateOne",
            json!({ "where": { "tenant_id": { "tenant": 1, "id": 41 } }, "data": data }),
            json!({ "tenant": true, "id": true, "value": true }),
        );
        assert_returned_identifier(&plan, "FB_COMPOUND", &["TENANT", "ID"]);
        assert_eq!(statements(&plan).len(), 3, "{plan:#}");
        for statement in statements(&plan) {
            assert!(
                !sql(statement).contains("\"TENANT\", \"FB_COMPOUND\".\"ID\") IN"),
                "{plan:#}"
            );
        }
    }
}

#[test]
fn update_many_can_change_primary_keys_without_returning_or_client_side_cascades() {
    let plan = compile(
        "updateMany",
        json!({ "where": { "id": { "gte": 41 } }, "data": { "id": { "increment": 100 } } }),
        json!({ "count": true }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 1, "{plan:#}");
    let query = sql(statements[0]);
    assert!(
        query.starts_with("UPDATE \"FB_ITEM\" SET \"ID\" = (\"FB_ITEM\".\"ID\" + ?)"),
        "{query}"
    );
    assert!(!query.contains("RETURNING"), "{query}");
    assert_eq!(statements[0]["args"], json!(["100", "41"]));
    assert!(expressions(&plan, "transaction").is_empty(), "{plan:#}");
}

#[test]
fn upsert_update_branch_returns_a_changed_primary_key_in_the_same_transaction() {
    let plan = compile_model(
        "MutableCode",
        "upsertOne",
        json!({ "where": { "code": "UN" }, "create": { "code": "UN" }, "update": { "code": "CX" } }),
        json!({ "code": true, "description": true }),
    );
    assert_returned_identifier(&plan, "FB_MUTABLE_CODE", &["CODIGO"]);
    assert!(
        statements(&plan)
            .iter()
            .any(|statement| sql(statement).starts_with("INSERT INTO "))
    );
}

#[test]
fn nested_primary_key_update_returns_the_database_identifier_in_a_transaction() {
    let plan = compile_model(
        "MutableChild",
        "updateOne",
        json!({ "where": { "id": 1 }, "data": { "parent": { "update": { "code": "CX" } } } }),
        json!({ "id": true, "parentCode": true, "parent": { "selection": { "code": true } } }),
    );
    assert_returned_identifier(&plan, "FB_MUTABLE_CODE", &["CODIGO"]);
    let updates: Vec<_> = statements(&plan)
        .into_iter()
        .map(sql)
        .filter(|query| query.starts_with("UPDATE "))
        .collect();
    assert_eq!(updates.len(), 1, "{plan:#}");
    assert!(updates[0].starts_with("UPDATE \"FB_MUTABLE_CODE\""), "{plan:#}");
}

#[test]
fn primary_key_updates_still_reject_null_and_invalid_scalar_types_before_execution() {
    for input in [Value::Null, json!(true), json!({ "set": Value::Null })] {
        let error = try_compile_model(
            "MutableCode",
            "updateOne",
            json!({ "where": { "code": "UN" }, "data": { "code": input } }),
            json!({ "code": true }),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("code"), "{error}");
    }
}

#[test]
fn empty_updates_keep_the_identifier_initializer_without_a_dml_statement() {
    let plan = compile_model(
        "MutableCode",
        "updateOne",
        json!({ "where": { "code": "UN" }, "data": {} }),
        json!({ "code": true, "description": true }),
    );
    assert_eq!(expressions(&plan, "transaction").len(), 1, "{plan:#}");
    assert_eq!(expressions(&plan, "initializeRecord").len(), 1, "{plan:#}");
    assert!(expressions(&plan, "mapRecord").is_empty(), "{plan:#}");
    assert!(
        statements(&plan)
            .iter()
            .all(|statement| sql(statement).starts_with("SELECT ")),
        "{plan:#}"
    );
}

#[test]
fn extended_unique_filters_keep_their_pre_read_and_update_predicates() {
    let plan = compile_model(
        "MutableCode",
        "updateOne",
        json!({ "where": { "code": "UN", "description": "expected" }, "data": { "code": "CX" } }),
        json!({ "code": true, "description": true }),
    );
    assert_returned_identifier(&plan, "FB_MUTABLE_CODE", &["CODIGO"]);
    let statements = statements(&plan);
    assert_eq!(statements.len(), 3, "{plan:#}");
    let predicates: Vec<_> = statements
        .iter()
        .map(|statement| sql(statement))
        .filter(|query| query.contains("\"DESCRICAO\" = ?"))
        .collect();
    assert_eq!(predicates.len(), 2, "{plan:#}");
    assert!(!expressions(&plan, "validate").is_empty(), "{plan:#}");
}

#[test]
fn mysql_without_returning_retains_the_shared_in_memory_identifier_mapping() {
    let validated = psl::validate_without_extensions(
        "datasource db {\n provider = \"mysql\"\n}\nmodel Item {\n id Int @id\n value String?\n}".into(),
    );
    assert!(
        !validated.diagnostics.has_errors(),
        "{}",
        validated.render_own_diagnostics()
    );
    let plan = try_compile_with_schema(
        Arc::new(schema::build(Arc::new(validated), true)),
        SqlFamily::Mysql,
        "Item",
        "updateOne",
        json!({ "where": { "id": 41 }, "data": { "id": { "increment": 2 } } }),
        json!({ "id": true, "value": true }),
    )
    .unwrap();
    let mappings = expressions(&plan, "mapRecord");
    assert_eq!(mappings.len(), 1, "{plan:#}");
    assert_eq!(
        mappings[0]["args"]["fields"],
        json!({ "id": { "type": "add", "value": 2 } })
    );
    assert_eq!(expressions(&plan, "initializeRecord").len(), 1, "{plan:#}");
    assert!(
        statements(&plan)
            .iter()
            .all(|statement| !sql(statement).contains("RETURNING")),
        "{plan:#}"
    );
}

#[test]
fn create_uses_one_firebird_insert_returning_with_mapped_columns() {
    let plan = compile(
        "createOne",
        json!({ "data": { "id": 41, "code": "manual-id", "quantity": 7, "active": true } }),
        json!({ "id": true, "code": true, "quantity": true, "active": true }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 1, "{plan:#}");
    let query = sql(statements[0]);
    assert!(query.starts_with("INSERT INTO \"FB_ITEM\""), "{query}");
    assert!(
        query.contains("RETURNING \"ID\", \"CODE\", \"QUANTITY\", \"ACTIVE\""),
        "{query}"
    );
    assert!(!query.contains("public"), "{query}");
    assert!(!query.contains("RETURNING \"FB_ITEM\"."), "{query}");
    assert_eq!(statements[0]["args"], json!(["41", "manual-id", "7", true]));
}

#[test]
fn current_date_create_omits_the_generated_identifier_and_requests_singleton_returning() {
    let plan = compile_model(
        "DateExprProbe",
        "createOne",
        json!({ "data": { "createdDate": { "currentDate": true }, "status": "1" } }),
        json!({ "id": true, "createdDate": true }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 1, "{plan:#}");
    let query = sql(statements[0]);
    let (insert, returning) = query.split_once(" RETURNING ").unwrap();
    assert!(insert.starts_with("INSERT INTO \"DATE_EXPR_PROBE\""), "{query}");
    assert!(insert.contains("\"CREATED_DATE\""), "{query}");
    assert!(!insert.contains("\"ID\"") && !insert.contains("DEFAULT"), "{query}");
    assert_eq!(insert.matches("CURRENT_DATE").count(), 1, "{query}");
    assert_eq!(returning, "\"ID\", \"CREATED_DATE\"");
    assert_eq!(statements[0]["args"], json!(["1"]));
    assert!(expressions(&plan, "initializeRecord").is_empty(), "{plan:#}");
}

#[test]
fn current_date_update_keeps_other_assignments_returning_and_the_transactional_result_read() {
    let plan = compile_model(
        "DateExprProbe",
        "updateOne",
        json!({ "where": { "id": 41 }, "data": { "usedDate": { "currentDate": true }, "status": "2" } }),
        json!({ "id": true, "usedDate": true, "status": true }),
    );
    assert_returned_identifier(&plan, "DATE_EXPR_PROBE", &["ID"]);
    let plan_statements = statements(&plan);
    assert_eq!(plan_statements.len(), 3, "{plan:#}");
    let update = plan_statements
        .iter()
        .find(|statement| sql(statement).starts_with("UPDATE "))
        .unwrap();
    let query = sql(update);
    assert!(query.contains("\"USED_DATE\" = CURRENT_DATE"), "{query}");
    assert!(query.contains("\"STATUS\" = ?"), "{query}");
    let ordinary_plan = compile_model(
        "DateExprProbe",
        "updateOne",
        json!({ "where": { "id": 41 }, "data": { "usedDate": "2024-03-04T00:00:00.000Z", "status": "2" } }),
        json!({ "id": true, "usedDate": true, "status": true }),
    );
    let ordinary_statements = statements(&ordinary_plan);
    let ordinary_update = ordinary_statements
        .iter()
        .find(|statement| sql(statement).starts_with("UPDATE "))
        .unwrap();
    let ordinary_args = ordinary_update["args"].as_array().unwrap();
    let expression_args = update["args"].as_array().unwrap();
    assert_eq!(expression_args.len(), 3, "{plan:#}");
    assert_eq!(
        expression_args,
        &ordinary_args[1..],
        "status and both identifier predicates must stay bound"
    );
    assert_eq!(expression_args[0], json!("2"), "{plan:#}");
    assert!(!expression_args.contains(&json!(true)), "{plan:#}");
    assert_eq!(query.matches("CURRENT_DATE").count(), 1, "{query}");
}

#[test]
fn current_date_create_many_keeps_singleton_statements_mixed_values_and_shape_splitting() {
    let plan = compile_model(
        "DateExprProbe",
        "createMany",
        json!({ "data": [
            { "createdDate": { "currentDate": true }, "status": "1" },
            { "id": 42, "createdDate": "2024-03-04T00:00:00.000Z", "usedDate": { "currentDate": true }, "status": "2" },
            { "createdDate": { "currentDate": true }, "usedDate": null, "status": "3" },
        ] }),
        json!({ "count": true }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 3, "{plan:#}");
    assert_eq!(expressions(&plan, "transaction").len(), 1, "{plan:#}");
    assert_eq!(expressions(&plan, "execute").len(), 3, "{plan:#}");
    for statement in statements {
        let query = sql(statement);
        let args = statement["args"].as_array().unwrap();
        assert!(query.starts_with("INSERT INTO \"DATE_EXPR_PROBE\""), "{query}");
        assert_eq!(query.matches("CURRENT_DATE").count(), 1, "{query}");
        assert!(
            !query.contains("RETURNING") && !query.contains("DEFAULT") && !query.contains("), ("),
            "{query}"
        );
        assert_eq!(query.contains("\"ID\""), args.contains(&json!("2")), "{query}");
        assert!(
            !args.contains(&json!(true)) && !args.contains(&json!("CURRENT_DATE")),
            "{plan:#}"
        );
    }
}

#[test]
fn current_date_update_many_uses_no_returning_or_server_clock_bind() {
    let plan = compile_model(
        "DateExprProbe",
        "updateMany",
        json!({ "where": { "id": { "gte": 41 } }, "data": { "usedDate": { "currentDate": true }, "status": "2" } }),
        json!({ "count": true }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 1, "{plan:#}");
    let query = sql(statements[0]);
    assert!(query.contains("\"USED_DATE\" = CURRENT_DATE"), "{query}");
    assert!(query.contains("\"STATUS\" = ?"), "{query}");
    assert!(!query.contains("RETURNING"), "{query}");
    assert_eq!(statements[0]["args"].as_array().unwrap().len(), 2, "{plan:#}");
}

#[test]
fn current_date_upsert_stays_in_each_existing_transactional_write_branch() {
    let plan = compile_model(
        "DateExprProbe",
        "upsertOne",
        json!({
            "where": { "id": 41 },
            "create": { "createdDate": { "currentDate": true }, "status": "1" },
            "update": { "usedDate": { "currentDate": true }, "status": "2" },
        }),
        json!({ "id": true, "createdDate": true, "usedDate": true }),
    );
    assert_returned_identifier(&plan, "DATE_EXPR_PROBE", &["ID"]);
    let queries: Vec<_> = statements(&plan).into_iter().map(sql).collect();
    let writes: Vec<_> = queries
        .iter()
        .filter(|query| query.starts_with("INSERT INTO ") || query.starts_with("UPDATE "))
        .collect();
    assert_eq!(writes.len(), 2, "{plan:#}");
    assert!(
        writes.iter().all(|query| query.matches("CURRENT_DATE").count() == 1),
        "{plan:#}"
    );
    assert!(queries.iter().all(|query| !query.contains("ON CONFLICT")), "{plan:#}");
}

#[test]
fn current_date_nested_creates_and_updates_keep_the_existing_relation_plan() {
    let plan = compile_model(
        "DateExprProbe",
        "createOne",
        json!({ "data": {
            "createdDate": { "currentDate": true },
            "children": { "create": { "id": 7, "createdDate": { "currentDate": true } } },
        } }),
        json!({ "id": true, "createdDate": true }),
    );
    assert_eq!(expressions(&plan, "transaction").len(), 1, "{plan:#}");
    let inserts: Vec<_> = statements(&plan)
        .into_iter()
        .map(sql)
        .filter(|query| query.starts_with("INSERT INTO "))
        .collect();
    assert_eq!(inserts.len(), 2, "{plan:#}");
    assert!(
        inserts.iter().all(|query| query.matches("CURRENT_DATE").count() == 1),
        "{plan:#}"
    );
    assert!(
        inserts.iter().all(|query| query.contains("RETURNING \"ID\"")),
        "{plan:#}"
    );

    let plan = compile_model(
        "DateExprChild",
        "updateOne",
        json!({ "where": { "id": 7 }, "data": { "probe": { "update": { "usedDate": { "currentDate": true } } } } }),
        json!({ "id": true, "probeId": true }),
    );
    assert_returned_identifier(&plan, "DATE_EXPR_PROBE", &["ID"]);
    let updates: Vec<_> = statements(&plan)
        .into_iter()
        .map(sql)
        .filter(|query| query.starts_with("UPDATE "))
        .collect();
    assert_eq!(updates.len(), 1, "{plan:#}");
    assert!(updates[0].contains("\"USED_DATE\" = CURRENT_DATE"), "{plan:#}");
}

#[test]
fn current_date_primary_key_update_uses_the_database_date_for_the_result_read() {
    let plan = compile_model(
        "DateKeyProbe",
        "updateOne",
        json!({
            "where": { "date": "2024-03-04T00:00:00.000Z" },
            "data": { "date": { "currentDate": true }, "value": "changed" },
        }),
        json!({ "date": true, "value": true }),
    );
    assert_returned_identifier(&plan, "DATE_KEY_PROBE", &["DATE_KEY"]);
    let updates: Vec<_> = statements(&plan)
        .into_iter()
        .map(sql)
        .filter(|query| query.starts_with("UPDATE "))
        .collect();
    assert_eq!(updates.len(), 1, "{plan:#}");
    assert!(updates[0].contains("\"DATE_KEY\" = CURRENT_DATE"), "{plan:#}");
}

#[test]
fn current_date_inputs_preserve_iso_tagged_dates_nullable_values_and_set_updates() {
    for value in [
        json!("2024-03-04T00:00:00.000Z"),
        json!({ "$type": "DateTime", "value": "2024-03-04T00:00:00.000Z" }),
        json!({ "$type": "Param", "value": { "name": "dateValue", "type": "DateTime" } }),
    ] {
        let plan = compile_model(
            "DateExprProbe",
            "createOne",
            json!({ "data": { "createdDate": value, "usedDate": null } }),
            json!({ "id": true, "createdDate": true, "usedDate": true }),
        );
        assert!(
            statements(&plan)
                .iter()
                .all(|statement| !sql(statement).contains("CURRENT_DATE")),
            "{plan:#}"
        );
    }

    for value in [
        json!("2024-03-04T00:00:00.000Z"),
        json!({ "set": "2024-03-04T00:00:00.000Z" }),
        Value::Null,
        json!({ "set": null }),
    ] {
        let plan = compile_model(
            "DateExprProbe",
            "updateOne",
            json!({ "where": { "id": 41 }, "data": { "usedDate": value } }),
            json!({ "id": true, "usedDate": true }),
        );
        assert_returned_identifier(&plan, "DATE_EXPR_PROBE", &["ID"]);
        assert!(
            statements(&plan)
                .iter()
                .all(|statement| !sql(statement).contains("CURRENT_DATE")),
            "{plan:#}"
        );
    }
}

#[test]
fn current_date_write_inputs_reject_invalid_or_ambiguous_envelopes_and_plain_sql_strings() {
    for value in [
        json!("CURRENT_DATE"),
        json!(true),
        json!({}),
        json!({ "currentDate": false }),
        json!({ "currentDate": "true" }),
        json!({ "currentDate": null }),
        json!({ "currentDate": true, "set": "2024-03-04T00:00:00.000Z" }),
        json!({ "currentDate": true, "unknown": true }),
        json!({ "set": { "currentDate": true } }),
        json!({ "currentDate": { "$type": "Param", "value": { "name": "flag", "type": "Boolean" } } }),
    ] {
        for action in ["createOne", "updateOne"] {
            let arguments = if action == "createOne" {
                json!({ "data": { "createdDate": value } })
            } else {
                json!({ "where": { "id": 41 }, "data": { "createdDate": value } })
            };
            let result = try_compile_model("DateExprProbe", action, arguments, json!({ "id": true }));
            assert!(result.is_err(), "{action} accepted {value}: {result:?}");
        }
    }

    for arguments in [json!({ "data": {} }), json!({ "data": { "createdDate": null } })] {
        assert!(try_compile_model("DateExprProbe", "createOne", arguments, json!({ "id": true })).is_err());
    }
    for value in [Value::Null, json!({ "set": null })] {
        assert!(
            try_compile_model(
                "DateExprProbe",
                "updateOne",
                json!({ "where": { "id": 41 }, "data": { "createdDate": value } }),
                json!({ "id": true }),
            )
            .is_err()
        );
    }
}

#[test]
fn current_date_native_date_inputs_remain_distinct_from_other_datetime_native_types_in_both_orders() {
    for (first, second) in [("date", "timestamp"), ("timestamp", "date")] {
        let fields = [first, second]
            .into_iter()
            .flat_map(|name| {
                let native = if name == "date" { "Date" } else { "Timestamp" };
                [
                    format!("  {name} DateTime @db.{native}"),
                    format!("  nullable{name} DateTime? @db.{native}"),
                ]
            })
            .collect::<Vec<_>>()
            .join("\n");
        let validated = psl::validate_without_extensions(
            format!("datasource db {{\n provider = \"firebird\"\n}}\nmodel Probe {{\n id Int @id\n{fields}\n defaultTime DateTime?\n clockTime DateTime? @db.Time\n}}").into(),
        );
        assert!(
            !validated.diagnostics.has_errors(),
            "{}",
            validated.render_own_diagnostics()
        );
        let schema = Arc::new(schema::build(Arc::new(validated), true));
        let mutation = schema.find_mutation_field("updateOneProbe").unwrap();
        let data = mutation
            .arguments()
            .iter()
            .find(|argument| argument.name == "data")
            .unwrap()
            .field_types()
            .iter()
            .find_map(schema::InputType::as_object)
            .unwrap();
        for (date, timestamp) in [("date", "timestamp"), ("nullabledate", "nullabletimestamp")] {
            let operations = |name: &str| {
                data.get_fields()
                    .iter()
                    .find(|field| field.name == name)
                    .unwrap()
                    .field_types()
                    .iter()
                    .find_map(schema::InputType::as_object)
                    .unwrap()
            };
            let date_operations = operations(date);
            let timestamp_operations = operations(timestamp);
            assert_ne!(
                date_operations.identifier.name(),
                timestamp_operations.identifier.name()
            );
            let current_date = date_operations
                .get_fields()
                .iter()
                .find(|field| field.name == "currentDate")
                .unwrap();
            assert!(!current_date.is_parameterizable());
            assert!(
                timestamp_operations
                    .get_fields()
                    .iter()
                    .all(|field| field.name != "currentDate")
            );
        }
        for field in [
            "date",
            "nullabledate",
            "timestamp",
            "nullabletimestamp",
            "defaultTime",
            "clockTime",
        ] {
            let mut data = serde_json::Map::new();
            data.insert(field.to_owned(), json!({ "currentDate": true }));
            let result = try_compile_with_schema(
                schema.clone(),
                SqlFamily::Firebird,
                "Probe",
                "updateOne",
                json!({ "where": { "id": 41 }, "data": data }),
                json!({ "id": true }),
            );
            assert_eq!(
                result.is_ok(),
                field == "date" || field == "nullabledate",
                "{first}/{field}: {result:?}"
            );
        }
    }

    for field in ["timestamp", "time"] {
        let mut data = serde_json::Map::new();
        data.insert("createdDate".to_owned(), json!({ "currentDate": true }));
        data.insert(field.to_owned(), json!({ "currentDate": true }));
        assert!(
            try_compile_model(
                "DateExprProbe",
                "createOne",
                json!({ "data": data }),
                json!({ "id": true })
            )
            .is_err(),
            "createOne accepted {field}"
        );
    }
}

#[test]
fn current_date_write_input_remains_unavailable_on_other_connectors_with_native_date() {
    for (provider, family) in [("postgresql", SqlFamily::Postgres), ("mysql", SqlFamily::Mysql)] {
        let validated = psl::validate_without_extensions(
            format!("datasource db {{\n provider = \"{provider}\"\n}}\nmodel Probe {{\n id Int @id\n date DateTime @db.Date\n}}").into(),
        );
        assert!(
            !validated.diagnostics.has_errors(),
            "{}",
            validated.render_own_diagnostics()
        );
        let schema = Arc::new(schema::build(Arc::new(validated), true));
        for action in ["createOne", "updateOne"] {
            let arguments = if action == "createOne" {
                json!({ "data": { "id": 41, "date": { "currentDate": true } } })
            } else {
                json!({ "where": { "id": 41 }, "data": { "date": { "currentDate": true } } })
            };
            assert!(
                try_compile_with_schema(
                    schema.clone(),
                    family,
                    "Probe",
                    action,
                    arguments,
                    json!({ "id": true })
                )
                .is_err(),
                "{provider}/{action}"
            );
        }
    }
}

#[test]
fn find_unique_uses_quoted_firebird_identifiers_and_a_bound_id() {
    let plan = compile(
        "findUnique",
        json!({ "where": { "id": 41 } }),
        json!({ "id": true, "code": true }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 1, "{plan:#}");
    let query = sql(statements[0]);
    assert!(query.contains("FROM \"FB_ITEM\""), "{query}");
    assert!(query.contains("\"FB_ITEM\".\"ID\" = ?"), "{query}");
    assert!(!query.contains("LIMIT"), "{query}");
    assert_eq!(statements[0]["args"][0], "41");
}

#[test]
fn pagination_uses_firebird_offset_fetch_with_offset_parameter_first() {
    let plan = compile(
        "findMany",
        json!({ "orderBy": { "id": "asc" }, "skip": 2, "take": 3 }),
        json!({ "id": true, "code": true }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 1, "{plan:#}");
    let query = sql(statements[0]);
    assert!(query.contains("ORDER BY \"FB_ITEM\".\"ID\" ASC"), "{query}");
    assert!(query.ends_with("OFFSET ? ROWS FETCH NEXT ? ROWS ONLY"), "{query}");
    assert_eq!(statements[0]["args"], json!(["2", "3"]));
}

#[test]
fn update_one_reads_and_updates_inside_a_transaction_without_bulk_returning() {
    let plan = compile(
        "updateOne",
        json!({ "where": { "id": 41 }, "data": { "quantity": { "increment": 2 } } }),
        json!({ "id": true, "quantity": true }),
    );
    let statements = statements(&plan);
    let queries: Vec<_> = statements.iter().map(|statement| sql(statement)).collect();
    assert!(queries.iter().any(|query| query.starts_with("SELECT ")), "{plan:#}");
    let update = queries.iter().find(|query| query.starts_with("UPDATE ")).unwrap();
    assert!(update.starts_with("UPDATE \"FB_ITEM\" SET \"QUANTITY\" = "), "{update}");
    assert!(update.ends_with(" RETURNING \"ID\""), "{update}");
    assert!(plan.to_string().contains("\"type\":\"transaction\""), "{plan:#}");
}

#[test]
fn delete_one_reads_the_result_in_a_transaction_before_deleting() {
    let plan = compile(
        "deleteOne",
        json!({ "where": { "id": 41 } }),
        json!({ "id": true, "code": true }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 2, "{plan:#}");
    let queries: Vec<_> = statements.iter().map(|statement| sql(statement)).collect();
    assert!(queries.iter().any(|query| query.starts_with("SELECT ")), "{plan:#}");
    let delete = queries.iter().find(|query| query.starts_with("DELETE ")).unwrap();
    assert!(delete.starts_with("DELETE FROM \"FB_ITEM\""), "{delete}");
    assert!(!delete.contains("RETURNING"), "{delete}");
    assert!(plan.to_string().contains("\"type\":\"transaction\""), "{plan:#}");
}

#[test]
fn bulk_update_and_delete_do_not_request_firebird3_multi_row_returning() {
    for (action, arguments, prefix) in [
        (
            "updateMany",
            json!({ "where": { "active": true }, "data": { "quantity": 4 } }),
            "UPDATE ",
        ),
        ("deleteMany", json!({ "where": { "active": false } }), "DELETE "),
    ] {
        let plan = compile(action, arguments, json!({ "count": true }));
        let statements = statements(&plan);
        assert_eq!(statements.len(), 1, "{plan:#}");
        let query = sql(statements[0]);
        assert!(query.starts_with(prefix), "{query}");
        assert!(!query.contains("RETURNING"), "{query}");
    }
}

#[test]
fn upsert_uses_the_transactional_read_create_update_plan() {
    let plan = compile(
        "upsertOne",
        json!({
            "where": { "code": "upsert-key" },
            "create": { "id": 42, "code": "upsert-key", "quantity": 1 },
            "update": { "quantity": { "increment": 1 } },
        }),
        json!({ "id": true, "code": true, "quantity": true }),
    );
    let statements = statements(&plan);
    let queries: Vec<_> = statements.iter().map(|statement| sql(statement)).collect();
    for prefix in ["SELECT ", "INSERT INTO \"FB_ITEM\"", "UPDATE \"FB_ITEM\""] {
        assert!(queries.iter().any(|query| query.starts_with(prefix)), "{plan:#}");
    }
    assert!(queries.iter().all(|query| !query.contains("ON CONFLICT")), "{plan:#}");
    assert!(plan.to_string().contains("\"type\":\"transaction\""), "{plan:#}");
}

#[test]
fn unsupported_bulk_returning_and_join_strategy_stay_hidden() {
    let schema = query_schema();
    assert!(schema.find_mutation_field("createOneItem").is_some());
    assert!(schema.find_mutation_field("createManyItem").is_some());
    for name in ["createManyItemAndReturn", "updateManyItemAndReturn"] {
        assert!(
            schema.find_mutation_field(name).is_none(),
            "unexpected Firebird operation: {name}"
        );
    }
    assert!(!schema.can_resolve_relation_with_joins());
    assert!(!schema.can_native_upsert());
}

#[test]
fn omitted_integer_and_bigint_identity_values_are_returned_by_the_insert() {
    for (model, table) in [("AutoItem", "FB_AUTO"), ("AutoBigInt", "FB_AUTO_BIGINT")] {
        let plan = compile_model(
            model,
            "createOne",
            json!({ "data": { "code": "generated" } }),
            json!({ "id": true, "code": true }),
        );
        let statements = statements(&plan);
        assert_eq!(statements.len(), 1, "{plan:#}");
        let query = sql(statements[0]);
        let (insert, returning) = query.split_once(" RETURNING ").unwrap();
        assert!(insert.starts_with(&format!("INSERT INTO \"{table}\"")), "{query}");
        assert!(!insert.contains("\"ID\""), "{query}");
        assert_eq!(returning, "\"ID\", \"CODE\"");
        assert!(!plan.to_string().contains("lastInsertId"), "{plan:#}");
    }
}

#[test]
fn an_identity_only_create_uses_default_values_and_singleton_returning() {
    let plan = compile_model("AutoOnly", "createOne", json!({ "data": {} }), json!({ "id": true }));
    let statements = statements(&plan);
    assert_eq!(statements.len(), 1, "{plan:#}");
    assert_eq!(
        sql(statements[0]),
        "INSERT INTO \"FB_AUTO_ONLY\" DEFAULT VALUES RETURNING \"ID\""
    );
    assert_eq!(statements[0]["args"], json!([]));
}

#[test]
fn create_many_uses_atomic_single_row_inserts_and_sums_their_affected_counts() {
    let plan = compile(
        "createMany",
        json!({ "data": [{ "id": 51, "code": "first" }, { "id": 52, "code": "second" }] }),
        json!({ "count": true }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 2, "{plan:#}");
    for statement in statements {
        let query = sql(statement);
        assert!(query.starts_with("INSERT INTO \"FB_ITEM\""), "{query}");
        assert!(!query.contains("RETURNING"), "{query}");
        assert!(!query.contains("), ("), "{query}");
    }
    let serialized = plan.to_string();
    assert!(serialized.contains("\"type\":\"transaction\""), "{plan:#}");
    assert!(serialized.contains("\"type\":\"sum\""), "{plan:#}");
    assert_eq!(serialized.matches("\"type\":\"execute\"").count(), 2, "{plan:#}");
}

#[test]
fn create_many_splits_explicit_and_omitted_identities_without_default_expressions() {
    let plan = compile_model(
        "AutoItem",
        "createMany",
        json!({ "data": [
            { "code": "generated-first" },
            { "id": 200, "code": "explicit" },
            { "code": "generated-second" },
        ] }),
        json!({ "count": true }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 3, "{plan:#}");
    for statement in statements {
        let query = sql(statement);
        let args = statement["args"].as_array().unwrap();
        assert!(!query.contains("DEFAULT"), "{query}");
        assert!(!query.contains("RETURNING"), "{query}");
        assert_eq!(query.contains("\"ID\""), args.contains(&json!("explicit")), "{query}");
    }
    assert!(plan.to_string().contains("\"type\":\"transaction\""), "{plan:#}");
}

#[test]
fn create_many_uses_null_for_missing_nullable_fields_in_the_same_shape() {
    let plan = compile_model(
        "AutoItem",
        "createMany",
        json!({ "data": [{ "code": "with-note", "note": "value" }, { "code": "without-note" }] }),
        json!({ "count": true }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 2, "{plan:#}");
    assert!(
        statements
            .iter()
            .any(|statement| sql(statement).to_ascii_uppercase().contains("NULL")),
        "{plan:#}"
    );
    assert!(
        statements.iter().all(|statement| !sql(statement).contains("DEFAULT")),
        "{plan:#}"
    );
}

#[test]
fn create_many_identity_only_rows_each_use_default_values_without_returning() {
    let plan = compile_model(
        "AutoOnly",
        "createMany",
        json!({ "data": [{}, {}, {}] }),
        json!({ "count": true }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 3, "{plan:#}");
    for statement in statements {
        assert_eq!(sql(statement), "INSERT INTO \"FB_AUTO_ONLY\" DEFAULT VALUES");
        assert_eq!(statement["args"], json!([]));
    }
    assert!(plan.to_string().contains("\"type\":\"transaction\""), "{plan:#}");
}

#[test]
fn create_many_empty_input_executes_no_database_statements() {
    let plan = compile_model(
        "AutoOnly",
        "createMany",
        json!({ "data": [] }),
        json!({ "count": true }),
    );
    assert!(statements(&plan).is_empty(), "{plan:#}");
    assert!(!plan.to_string().contains("\"type\":\"execute\""), "{plan:#}");
}

#[test]
fn create_many_rejects_skip_duplicates() {
    let error = try_compile_model(
        "AutoItem",
        "createMany",
        json!({ "data": [{ "code": "duplicate" }], "skipDuplicates": true }),
        json!({ "count": true }),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("skipDuplicates"), "{error}");
}

#[test]
fn existing_returning_connectors_keep_their_bulk_returning_api() {
    for provider in ["postgresql", "cockroachdb", "sqlite"] {
        let schema = psl::validate_without_extensions(
            format!("datasource db {{\n  provider = \"{provider}\"\n}}\nmodel Item {{\n  id Int @id\n}}").into(),
        );
        assert!(!schema.diagnostics.has_errors(), "{}", schema.render_own_diagnostics());
        let schema = schema::build(Arc::new(schema), true);
        assert!(
            schema.find_mutation_field("createManyItemAndReturn").is_some(),
            "{provider}"
        );
    }
}

#[test]
fn transaction_isolation_enum_matches_the_firebird_adapter() {
    let schema = query_schema();
    let Some(schema::EnumType::String(levels)) = schema::itx_isolation_levels(&schema) else {
        panic!("expected a Firebird transaction isolation enum");
    };
    assert_eq!(
        levels.values(),
        ["ReadCommitted", "RepeatableRead", "Serializable", "Snapshot"]
    );
    assert_eq!(levels.value_for("ReadUncommitted"), None);
}

#[test]
fn compound_id_read_and_update_use_scalar_predicates() {
    for (action, arguments, prefix) in [
        (
            "findUnique",
            json!({ "where": { "tenant_id": { "tenant": 1, "id": 41 } } }),
            "SELECT ",
        ),
        (
            "updateOne",
            json!({ "where": { "tenant_id": { "tenant": 1, "id": 41 } }, "data": { "value": "changed" } }),
            "UPDATE ",
        ),
    ] {
        let plan = compile_model(
            "CompoundItem",
            action,
            arguments,
            json!({ "tenant": true, "id": true, "value": true }),
        );
        let statements = statements(&plan);
        let queries: Vec<_> = statements.iter().map(|statement| sql(statement)).collect();
        assert!(queries.iter().any(|query| query.starts_with(prefix)), "{plan:#}");
        assert!(
            queries
                .iter()
                .all(|query| !query.contains("\"TENANT\", \"FB_COMPOUND\".\"ID\") IN")),
            "{plan:#}"
        );
    }
}

#[test]
fn integer_average_casts_to_double_precision_to_preserve_fractional_results() {
    let plan = compile(
        "aggregate",
        json!({}),
        json!({ "_avg": { "selection": { "quantity": true } } }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 1, "{plan:#}");
    let query = sql(statements[0]);
    assert!(query.contains("AVG(CAST("), "{query}");
    assert!(query.contains("\"QUANTITY\" AS DOUBLE PRECISION))"), "{query}");
}

#[test]
fn integer_average_in_having_also_preserves_fractional_results() {
    let plan = compile(
        "groupBy",
        json!({ "by": ["active"], "having": { "quantity": { "_avg": { "gt": 1.5 } } } }),
        json!({ "active": true, "_avg": { "selection": { "quantity": true } } }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 1, "{plan:#}");
    let query = sql(statements[0]);
    let (_, having) = query
        .split_once(" HAVING ")
        .expect("expected an aggregate HAVING clause");
    assert!(query.contains("AVG(CAST("), "{query}");
    assert!(having.contains("AVG(CAST("), "{query}");
    assert!(having.contains("\"QUANTITY\" AS DOUBLE PRECISION))"), "{query}");
}

#[test]
fn decimal_average_fails_explicitly_instead_of_returning_a_rounded_value() {
    let error = try_compile_model(
        "Item",
        "aggregate",
        json!({}),
        json!({ "_avg": { "selection": { "amount": true } } }),
    )
    .unwrap_err()
    .to_string()
    .to_ascii_lowercase();
    assert!(error.contains("avg") && error.contains("decimal"), "{error}");
}

#[test]
fn aggregate_aliases_over_the_firebird_identifier_limit_fail_before_execution() {
    for selection in [
        json!({ "_count": { "selection": { "longMappedField": true } } }),
        json!({ "_sum": { "selection": { "longMappedField": true } } }),
    ] {
        // The underlying mapped column is a valid 31-byte Firebird identifier,
        // but the generated `_count$` / `_sum$` prefix would exceed that limit.
        let error = try_compile_model("Item", "aggregate", json!({}), selection)
            .unwrap_err()
            .to_string()
            .to_ascii_lowercase();
        assert!(error.contains("alias") && error.contains("31"), "{error}");
    }
}

#[test]
fn literal_prefixes_compile_for_ibpt_and_cidades_counts_and_rows_without_changing_input() {
    let prefixes = [
        "%".to_owned(),
        "_".to_owned(),
        "A%_\\'".to_owned(),
        String::new(),
        "\u{00e9}\u{1f600}".to_owned(),
        "x".repeat(10),
        "x".repeat(11),
        "x".repeat(100),
        "\u{1f600}".repeat(8191),
        format!("{} ", "x".repeat(8191)),
    ];
    for (model, field, table, column) in [
        ("AliquotaIbpt", "codigo", "IBPT", "COD_NCM"),
        ("CidadeLegado", "municipio", "CIDADES", "NOME_MUNICIPIO"),
    ] {
        for prefix in &prefixes {
            for (action, selection) in [
                ("findMany", json!({ (field): true })),
                ("aggregate", json!({ "_count": { "selection": { "_all": true } } })),
            ] {
                let plan = compile_model(
                    model,
                    action,
                    json!({ "where": { (field): { "startsWith": prefix } } }),
                    selection,
                );
                let statements = statements(&plan);
                assert_eq!(statements.len(), 1, "{plan:#}");
                let query = sql(statements[0]);
                assert!(
                    query.contains(&format!(
                        "\"{table}\".\"{column}\" STARTING WITH CAST(? AS VARCHAR(8191) CHARACTER SET UTF8)"
                    )),
                    "{plan:#}"
                );
                assert!(
                    !query.contains(" LIKE ") && !query.contains(" || ") && !query.contains("ESCAPE"),
                    "{plan:#}"
                );
                assert_eq!(statements[0]["args"][0], json!(prefix), "{plan:#}");
                assert_eq!(statements[0]["argTypes"][0]["scalarType"], "string", "{plan:#}");
            }
        }
    }
}

#[test]
fn cached_prefix_parameters_keep_the_cast_at_the_bind_atom() {
    for filter in [
        json!({ "startsWith": { "$type": "Param", "value": { "name": "prefix", "type": "String" } } }),
        json!({ "not": { "startsWith": { "$type": "Param", "value": { "name": "prefix", "type": "String" } } } }),
    ] {
        let plan = compile_model(
            "AliquotaIbpt",
            "findMany",
            json!({ "where": { "codigo": filter } }),
            json!({ "codigo": true }),
        );
        let statements = statements(&plan);
        assert_eq!(statements.len(), 1, "{plan:#}");
        let query = sql(statements[0]);
        assert!(
            query.contains("STARTING WITH CAST(? AS VARCHAR(8191) CHARACTER SET UTF8)"),
            "{plan:#}"
        );
        assert!(!query.contains(" || ") && !query.contains(" LIKE "), "{plan:#}");
        assert_eq!(
            statements[0]["args"][0],
            json!({ "prisma__type": "param", "prisma__value": { "name": "prefix", "type": "String" } }),
            "{plan:#}"
        );
        assert_eq!(statements[0]["argTypes"][0]["scalarType"], "string", "{plan:#}");
    }
}

#[test]
fn prefix_not_and_outer_not_keep_their_boolean_structure_around_a_literal_prefix() {
    // A field's `not` negates the scalar condition. An outer `NOT` retains
    // the shared filter tree's boolean wrapper, including a double negation.
    for (filter, predicate) in [
        (
            json!({ "codigo": { "not": { "startsWith": "A%_" } } }),
            r#""IBPT"."COD_NCM" NOT STARTING WITH CAST(? AS VARCHAR(8191) CHARACTER SET UTF8)"#,
        ),
        (
            json!({ "NOT": { "codigo": { "startsWith": "A%_" } } }),
            r#"(NOT "IBPT"."COD_NCM" STARTING WITH CAST(? AS VARCHAR(8191) CHARACTER SET UTF8))"#,
        ),
        (
            json!({ "NOT": { "codigo": { "not": { "startsWith": "A%_" } } } }),
            r#"(NOT "IBPT"."COD_NCM" NOT STARTING WITH CAST(? AS VARCHAR(8191) CHARACTER SET UTF8))"#,
        ),
    ] {
        let plan = compile_model(
            "AliquotaIbpt",
            "findMany",
            json!({ "where": filter }),
            json!({ "codigo": true }),
        );
        let statements = statements(&plan);
        let query = sql(statements[0]);
        assert_eq!(
            query,
            format!(r#"SELECT "IBPT"."COD_NCM" FROM "IBPT" WHERE {predicate} OFFSET ? ROWS"#),
            "{plan:#}"
        );
        assert!(!query.contains(" LIKE "), "{plan:#}");
        assert_eq!(statements[0]["args"][0], "A%_", "{plan:#}");
    }
}

#[test]
fn prefix_field_references_use_physical_columns_without_patterns_or_parameter_casts() {
    for filter in [
        json!({ "startsWith": { "$type": "FieldRef", "value": { "_ref": "estado", "_container": "CidadeLegado" } } }),
        json!({ "not": { "startsWith": { "$type": "FieldRef", "value": { "_ref": "estado", "_container": "CidadeLegado" } } } }),
    ] {
        let plan = compile_model(
            "CidadeLegado",
            "findMany",
            json!({ "where": { "municipio": filter } }),
            json!({ "codigo": true, "municipio": true }),
        );
        let statements = statements(&plan);
        let query = sql(statements[0]);
        assert!(query.contains("STARTING WITH \"CIDADES\".\"ESTADO\""), "{plan:#}");
        assert!(
            !query.contains("CAST(") && !query.contains(" LIKE ") && !query.contains(" || "),
            "{plan:#}"
        );
        assert!(
            statements[0]["argTypes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|arg| arg["scalarType"] != "string"),
            "{plan:#}"
        );
    }
}

#[test]
fn cidades_uf_or_prefixes_have_independent_wide_binds_even_for_the_two_character_sigla() {
    let plan = compile_model(
        "CidadeLegado",
        "findMany",
        json!({ "where": { "OR": [{ "sigla": { "startsWith": "PER" } }, { "estado": { "startsWith": "PER" } }] } }),
        json!({ "codigo": true, "municipio": true, "estado": true, "sigla": true }),
    );
    let statements = statements(&plan);
    assert_eq!(statements.len(), 1, "{plan:#}");
    let query = sql(statements[0]);
    for column in ["SIGLA", "ESTADO"] {
        assert!(
            query.contains(&format!(
                "\"CIDADES\".\"{column}\" STARTING WITH CAST(? AS VARCHAR(8191) CHARACTER SET UTF8)"
            )),
            "{plan:#}"
        );
    }
    assert_eq!(query.matches("CAST(? AS VARCHAR(8191) CHARACTER SET UTF8)").count(), 2);
    assert_eq!(statements[0]["args"][0], "PER");
    assert_eq!(statements[0]["args"][1], "PER");
}

#[test]
fn current_and_historical_cidades_widths_do_not_limit_prefix_parameter_typing() {
    for (municipio_width, estado_width) in [(35, 25), (30, 20)] {
        let validated = psl::validate_without_extensions(
            format!(
                "datasource db {{\n provider = \"firebird\"\n}}\nmodel Cidade {{\n codigo Int @id @map(\"COD_MUN\")\n municipio String @db.VarChar({municipio_width}) @map(\"NOME_MUNICIPIO\")\n estado String @db.VarChar({estado_width}) @map(\"ESTADO\")\n sigla String @db.VarChar(2) @map(\"SIGLA\")\n @@map(\"CIDADES\")\n}}"
            )
            .into(),
        );
        assert!(
            !validated.diagnostics.has_errors(),
            "{}",
            validated.render_own_diagnostics()
        );
        let schema = Arc::new(schema::build(Arc::new(validated), true));
        for length in [municipio_width, municipio_width + 1] {
            let prefix = "x".repeat(length);
            let plan = try_compile_with_schema(
                schema.clone(),
                SqlFamily::Firebird,
                "Cidade",
                "findMany",
                json!({ "where": { "municipio": { "startsWith": prefix } } }),
                json!({ "codigo": true }),
            )
            .unwrap();
            let statements = statements(&plan);
            assert!(
                sql(statements[0]).contains("STARTING WITH CAST(? AS VARCHAR(8191) CHARACTER SET UTF8)"),
                "{plan:#}"
            );
            assert_eq!(statements[0]["args"][0], prefix, "{plan:#}");
        }
    }
}

#[test]
fn contains_and_ends_with_keep_their_existing_like_patterns_and_negations() {
    for (filter, pattern, negated) in [
        (json!({ "contains": "A%_\\" }), "%A%_\\%", false),
        (json!({ "endsWith": "A%_\\" }), "%A%_\\", false),
        (json!({ "not": { "contains": "A%_\\" } }), "%A%_\\%", true),
        (json!({ "not": { "endsWith": "A%_\\" } }), "%A%_\\", true),
    ] {
        let plan = compile_model(
            "AliquotaIbpt",
            "findMany",
            json!({ "where": { "codigo": filter } }),
            json!({ "codigo": true }),
        );
        let statements = statements(&plan);
        let query = sql(statements[0]);
        assert!(
            query.contains(if negated { " NOT LIKE " } else { " LIKE " }),
            "{plan:#}"
        );
        assert!(
            !query.contains("STARTING WITH") && !query.contains("CAST(") && !query.contains("ESCAPE"),
            "{plan:#}"
        );
        assert_eq!(statements[0]["args"][0], pattern, "{plan:#}");
    }
}

#[test]
fn firebird_prefix_filter_does_not_expose_insensitive_mode() {
    let error = try_compile_model(
        "AliquotaIbpt",
        "findMany",
        json!({ "where": { "codigo": { "startsWith": "A", "mode": "insensitive" } } }),
        json!({ "codigo": true }),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("mode"), "{error}");
}

#[test]
fn other_sql_providers_keep_their_existing_starts_with_like_contract() {
    for (provider, family) in [
        ("postgresql", SqlFamily::Postgres),
        ("mysql", SqlFamily::Mysql),
        ("sqlite", SqlFamily::Sqlite),
        ("sqlserver", SqlFamily::Mssql),
    ] {
        let validated = psl::validate_without_extensions(
            format!("datasource db {{\n provider = \"{provider}\"\n}}\nmodel Item {{\n id Int @id\n name String\n}}")
                .into(),
        );
        assert!(
            !validated.diagnostics.has_errors(),
            "{}",
            validated.render_own_diagnostics()
        );
        let plan = try_compile_with_schema(
            Arc::new(schema::build(Arc::new(validated), true)),
            family,
            "Item",
            "findMany",
            json!({ "where": { "name": { "startsWith": "A%_\\" } } }),
            json!({ "id": true }),
        )
        .unwrap();
        let statements = expressions(&plan, "templateSql");
        assert_eq!(statements.len(), 1, "{provider}: {plan:#}");
        let query = sql(statements[0]);
        assert!(query.contains(" LIKE "), "{provider}: {plan:#}");
        assert!(
            !query.contains("STARTING WITH") && !query.contains("VARCHAR(8191)"),
            "{provider}: {plan:#}"
        );
        assert_eq!(statements[0]["args"][0], "A%_\\%", "{provider}: {plan:#}");
    }
}
