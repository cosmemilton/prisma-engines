// Added for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.

use super::*;
use quaint::{Value, connector::ColumnType as ResultType};

fn result(fields: &[&str], rows: Vec<Vec<Value<'static>>>) -> ResultSet {
    ResultSet::new(
        fields.iter().map(|field| (*field).into()).collect(),
        vec![ResultType::Unknown; fields.len()],
        rows,
    )
}

fn null() -> Value<'static> {
    Value::null_text()
}

const COLUMN_FIELDS: &[&str] = &[
    "table_name",
    "column_name",
    "field_type",
    "field_sub_type",
    "field_precision",
    "field_scale",
    "character_length",
    "character_set",
    "collation_id",
    "not_null",
    "column_default",
    "domain_default",
    "identity_type",
    "generator_name",
    "field_source",
    "dimensions",
    "computed_source",
    "validation_source",
    "description",
    "is_computed",
    "has_validation",
    "has_default",
    "domain_not_null",
];

fn column(table: &str, name: &str, code: i64) -> Vec<Value<'static>> {
    vec![
        table.to_owned().into(),
        name.to_owned().into(),
        code.into(),
        0i64.into(),
        null(),
        0i64.into(),
        null(),
        null(),
        0i64.into(),
        0i64.into(),
        null(),
        null(),
        null(),
        null(),
        "RDB$1".into(),
        null(),
        null(),
        null(),
        null(),
        0i64.into(),
        0i64.into(),
        0i64.into(),
        0i64.into(),
    ]
}

fn set(row: &mut [Value<'static>], field: &str, value: impl Into<Value<'static>>) {
    row[COLUMN_FIELDS.iter().position(|name| *name == field).unwrap()] = value.into();
    if matches!(field, "column_default" | "domain_default") {
        row[COLUMN_FIELDS.iter().position(|name| *name == "has_default").unwrap()] = 1i64.into();
    }
}

const INDEX_FIELDS: &[&str] = &[
    "table_name",
    "index_name",
    "is_unique",
    "is_descending",
    "is_inactive",
    "expression_source",
    "foreign_key_index",
    "column_name",
    "position",
    "constraint_name",
    "constraint_type",
    "is_expression",
];

fn index(
    table: &str,
    name: &str,
    column: &str,
    position: i64,
    constraint: Option<(&str, &str)>,
) -> Vec<Value<'static>> {
    vec![
        table.to_owned().into(),
        name.to_owned().into(),
        i64::from(constraint.is_some()).into(),
        0i64.into(),
        0i64.into(),
        null(),
        null(),
        column.to_owned().into(),
        position.into(),
        constraint
            .map(|(name, _)| Value::from(name.to_owned()))
            .unwrap_or_else(null),
        constraint
            .map(|(_, kind)| Value::from(kind.to_owned()))
            .unwrap_or_else(null),
        0i64.into(),
    ]
}

#[derive(Default)]
struct Catalog {
    tables: Vec<Vec<Value<'static>>>,
    columns: Vec<Vec<Value<'static>>>,
    indexes: Vec<Vec<Value<'static>>>,
    foreign_keys: Vec<Vec<Value<'static>>>,
    checks: Vec<Vec<Value<'static>>>,
    generators: Vec<Vec<Value<'static>>>,
    triggers: Vec<Vec<Value<'static>>>,
}

impl Catalog {
    fn table(&mut self, name: &str) {
        self.tables.push(vec![name.to_owned().into(), 0i64.into(), null()]);
    }

    fn describe(self) -> DescriberResult<SqlSchema> {
        build_schema(
            result(&["table_name", "relation_type", "description"], self.tables),
            result(COLUMN_FIELDS, self.columns),
            result(INDEX_FIELDS, self.indexes),
            result(
                &[
                    "table_name",
                    "constraint_name",
                    "index_name",
                    "column_name",
                    "position",
                    "referenced_table",
                    "referenced_column",
                    "delete_rule",
                    "update_rule",
                ],
                self.foreign_keys,
            ),
            result(&["table_name", "constraint_name"], self.checks),
            result(
                &["generator_name", "system_flag", "initial_value", "increment"],
                self.generators,
            ),
            result(&["trigger_name", "table_name", "source", "inactive"], self.triggers),
        )
    }
}

fn one_column(code: i64, subtype: i64) -> Catalog {
    let mut catalog = Catalog::default();
    catalog.table("Mapped Table");
    let mut row = column("Mapped Table", "MixedCase", code);
    set(&mut row, "field_sub_type", subtype);
    catalog.columns.push(row);
    catalog
}

#[test]
fn firebird_preserves_native_type_families() {
    for (code, family, native) in [
        (7, ColumnTypeFamily::Int, FirebirdType::SmallInt),
        (8, ColumnTypeFamily::Int, FirebirdType::Integer),
        (16, ColumnTypeFamily::BigInt, FirebirdType::BigInt),
        (10, ColumnTypeFamily::Float, FirebirdType::Float),
        (27, ColumnTypeFamily::Float, FirebirdType::DoublePrecision),
        (23, ColumnTypeFamily::Boolean, FirebirdType::Boolean),
        (12, ColumnTypeFamily::DateTime, FirebirdType::Date),
        (13, ColumnTypeFamily::DateTime, FirebirdType::Time),
        (35, ColumnTypeFamily::DateTime, FirebirdType::Timestamp),
        (261, ColumnTypeFamily::Binary, FirebirdType::Blob),
    ] {
        let schema = one_column(code, 0).describe().unwrap();
        let column = schema.walk_table_columns().next().unwrap();
        assert_eq!(column.name(), "MixedCase");
        assert_eq!(column.column_type_family(), &family);
        assert_eq!(column.column_native_type::<FirebirdType>(), Some(&native));
        assert!(column.arity().is_nullable());
        assert_eq!(schema.table_walkers().next().unwrap().name(), "Mapped Table");
        assert_eq!(schema.namespaces_count(), 0);
        assert_eq!(schema.table_walkers().next().unwrap().namespace(), None);
    }
}

#[test]
fn firebird_numeric_subtype_precision_and_scale_are_exact() {
    for (subtype, native) in [
        (1, FirebirdType::Numeric(Some((18, 6)))),
        (2, FirebirdType::Decimal(Some((18, 6)))),
    ] {
        let mut catalog = one_column(16, subtype);
        set(&mut catalog.columns[0], "field_precision", 18i64);
        set(&mut catalog.columns[0], "field_scale", -6i64);
        set(&mut catalog.columns[0], "column_default", "DEFAULT 123456789012.123456");
        let schema = catalog.describe().unwrap();
        let column = schema.walk_table_columns().next().unwrap();
        assert_eq!(column.column_type_family(), &ColumnTypeFamily::Decimal);
        assert_eq!(column.column_native_type::<FirebirdType>(), Some(&native));
        assert_eq!(
            column.default().unwrap().as_value(),
            Some(&PrismaValue::Float(
                bigdecimal::BigDecimal::from_str("123456789012.123456").unwrap()
            ))
        );
    }
}

#[test]
fn firebird_text_lengths_are_characters_and_defaults_unescape_quotes() {
    for (code, subtype, expected) in [
        (14, 0, FirebirdType::Char(42)),
        (37, 0, FirebirdType::VarChar(42)),
        (261, 1, FirebirdType::Text),
    ] {
        let mut catalog = one_column(code, subtype);
        set(&mut catalog.columns[0], "character_length", 42i64);
        set(&mut catalog.columns[0], "character_set", "UTF8   ");
        set(&mut catalog.columns[0], "column_default", "DEFAULT 'it''s a value  '");
        let schema = catalog.describe().unwrap();
        let column = schema.walk_table_columns().next().unwrap();
        assert_eq!(column.column_native_type::<FirebirdType>(), Some(&expected));
        assert_eq!(
            column.default().unwrap().as_value(),
            Some(&PrismaValue::String("it's a value  ".into()))
        );
    }
}

#[test]
fn firebird_domain_defaults_and_requiredness_are_preserved() {
    let mut catalog = one_column(23, 0);
    set(&mut catalog.columns[0], "not_null", 1i64);
    set(&mut catalog.columns[0], "domain_default", "DEFAULT FALSE");
    set(&mut catalog.columns[0], "domain_not_null", 1i64);
    let schema = catalog.describe().unwrap();
    let column = schema.walk_table_columns().next().unwrap();
    assert!(column.arity().is_required());
    assert_eq!(column.default().unwrap().as_value(), Some(&PrismaValue::Boolean(false)));
    let ext: &FirebirdSchemaExt = schema.downcast_connector_data();
    assert!(ext.columns[0].1.domain_not_null);
    assert_eq!(ext.columns[0].1.domain_default.as_deref(), Some("DEFAULT FALSE"));
    assert_eq!(ext.columns[0].1.column_default, None);
}

#[test]
fn firebird_identity_is_explicit_and_ordinary_generator_trigger_is_not_identity() {
    let mut catalog = one_column(16, 0);
    set(&mut catalog.columns[0], "identity_type", 1i64);
    set(&mut catalog.columns[0], "generator_name", "RDB$IDENT_1  ");
    let ordinary = column("Mapped Table", "trigger_id", 8);
    catalog.columns.push(ordinary);
    catalog
        .generators
        .push(vec!["RDB$IDENT_1".into(), 6i64.into(), 100i64.into(), 1i64.into()]);
    catalog
        .generators
        .push(vec!["LEGACY_SEQ".into(), 0i64.into(), 0i64.into(), 1i64.into()]);
    catalog.triggers.push(vec![
        "SET_ID".into(),
        "Mapped Table".into(),
        "AS BEGIN NEW.trigger_id = NEXT VALUE FOR LEGACY_SEQ; END".into(),
        0i64.into(),
    ]);
    let schema = catalog.describe().unwrap();
    let columns: Vec<_> = schema.walk_table_columns().collect();
    assert!(columns[0].is_autoincrement());
    assert!(columns[0].arity().is_required());
    assert!(columns[0].default().is_none());
    assert!(!columns[1].is_autoincrement());
    assert!(columns[1].default().is_none());
    let ext: &FirebirdSchemaExt = schema.downcast_connector_data();
    assert_eq!(ext.identity(columns[0].id).unwrap().generator_name, "RDB$IDENT_1");
    assert_eq!(ext.generators[0].initial_value, 100);
    assert_eq!(ext.triggers.len(), 1);
}

#[test]
fn firebird_composite_keys_preserve_column_order_names_and_actions() {
    let mut catalog = Catalog::default();
    for table in ["Child", "Parent"] {
        catalog.table(table);
        for name in ["A", "B"] {
            catalog.columns.push(column(table, name, 8));
        }
    }
    catalog.indexes.push(index("Child", "IDX_CHILD", "B", 0, None));
    catalog.indexes[0][3] = 1i64.into();
    catalog.indexes.push(index(
        "Parent",
        "PHYSICAL_PK",
        "B",
        0,
        Some(("PK_Parent", "PRIMARY KEY")),
    ));
    catalog.indexes.push(index(
        "Parent",
        "PHYSICAL_PK",
        "A",
        1,
        Some(("PK_Parent", "PRIMARY KEY")),
    ));
    for (position, name) in [(0i64, "B"), (1, "A")] {
        catalog.foreign_keys.push(vec![
            "Child".into(),
            "FK_Child".into(),
            "FK_INDEX".into(),
            name.into(),
            position.into(),
            "Parent".into(),
            name.into(),
            "CASCADE   ".into(),
            "NO ACTION".into(),
        ]);
    }
    let schema = catalog.describe().unwrap();
    let parent = schema.table_walker("Parent").unwrap();
    let pk = parent.primary_key().unwrap();
    assert_eq!(pk.name(), "PK_Parent");
    assert_eq!(
        pk.columns().map(|column| column.as_column().name()).collect::<Vec<_>>(),
        ["B", "A"]
    );
    let fk = schema.walk_foreign_keys().next().unwrap();
    assert_eq!(
        fk.constrained_columns().map(|column| column.name()).collect::<Vec<_>>(),
        ["B", "A"]
    );
    assert_eq!(
        fk.referenced_columns().map(|column| column.name()).collect::<Vec<_>>(),
        ["B", "A"]
    );
    assert_eq!(fk.on_delete_action(), ForeignKeyAction::Cascade);
    assert_eq!(fk.on_update_action(), ForeignKeyAction::NoAction);
    let ext: &FirebirdSchemaExt = schema.downcast_connector_data();
    assert_eq!(ext.index_metadata(pk.id).unwrap().database_name, "PHYSICAL_PK");
    assert_eq!(ext.foreign_key_indexes[0].1, "FK_INDEX");
    assert_eq!(
        schema
            .table_walker("Child")
            .unwrap()
            .indexes()
            .next()
            .unwrap()
            .columns()
            .next()
            .unwrap()
            .sort_order(),
        Some(SQLSortOrder::Desc)
    );
}

#[test]
fn firebird_unique_constraint_and_standalone_unique_index_remain_distinct() {
    let mut catalog = one_column(8, 0);
    catalog.indexes.push(index(
        "Mapped Table",
        "BACKING",
        "MixedCase",
        0,
        Some(("UQ_Logical", "UNIQUE")),
    ));
    let mut standalone = index("Mapped Table", "UNIQUE_INDEX", "MixedCase", 0, None);
    standalone[2] = 1i64.into();
    catalog.indexes.push(standalone);
    let schema = catalog.describe().unwrap();
    let ext: &FirebirdSchemaExt = schema.downcast_connector_data();
    assert_eq!(ext.indexes[0].1.constraint_name.as_deref(), Some("UQ_Logical"));
    assert_eq!(ext.indexes[1].1.constraint_name, None);
}

#[test]
fn firebird_default_expressions_are_not_misparsed_as_literals() {
    for expression in [
        "CURRENT_USER",
        "CURRENT_ROLE",
        "CURRENT_TIMESTAMP(2)",
        "DATE '2026-10-18'",
        "_UTF8 'introduced'",
        "X'01ff'",
    ] {
        let default = parse_default(&format!("DEFAULT {expression}"), &ColumnTypeFamily::String).unwrap();
        assert_eq!(default.kind(), &DefaultKind::DbGenerated(Some(expression.into())));
    }
    assert!(matches!(
        parse_default("DEFAULT CURRENT_TIMESTAMP", &ColumnTypeFamily::DateTime)
            .unwrap()
            .kind(),
        DefaultKind::Now
    ));
    assert!(parse_default("CURRENT_TIMESTAMP", &ColumnTypeFamily::DateTime).is_err());
    for expression in [
        "'a' || 'b'",
        "NEXT VALUE FOR SEQ",
        "GEN_ID(SEQ, 1)",
        "('literal')",
        "GEN_UUID()",
    ] {
        let error = parse_default(&format!("DEFAULT {expression}"), &ColumnTypeFamily::String).unwrap_err();
        assert!(error.to_string().contains("defaults require a literal"));
    }
}

#[test]
fn firebird_default_grammar_accepts_literals_and_context_variables_only() {
    assert_eq!(
        parse_default("DEFAULT NULL", &ColumnTypeFamily::Int).unwrap().kind(),
        &DefaultKind::DbGenerated(Some("NULL".into()))
    );
    for expression in [
        "NULL",
        "TRUE",
        "false",
        "UNKNOWN",
        "42",
        "-123.456",
        "+1.5e-3",
        "0xFF",
        "'a; b''c'",
        "q'!That's a string!'",
        "Q'{a{b}c}'",
        "X''",
        "x'01ff'",
        "_UTF8'hello'",
        "_ASCII x'4162'",
        "DATE '2026-10-18'",
        "TIME '12:34:56.1234'",
        "TIMESTAMP '2026-10-18 12:34:56'",
        "USER",
        "CURRENT_CONNECTION",
        "CURRENT_TRANSACTION",
        "CURRENT_DATE",
        "CURRENT_TIME ( 3 )",
        "LOCALTIMESTAMP(2)",
        "LOCALTIME",
    ] {
        assert!(is_supported_default_expression(expression), "{expression}");
    }
    for expression in [
        "",
        "NEXT VALUE FOR S",
        "GEN_ID(S,1)",
        "GEN_UUID()",
        "'a' || 'b'",
        "1 + 1",
        "(42)",
        "CURRENT_TIMESTAMP(4)",
        "x'1'",
        "1_000",
        "'a'; DROP TABLE T",
        "q'!a!' || q'!b!'",
        "_UTF8 'a' || 'b'",
    ] {
        assert!(!is_supported_default_expression(expression), "{expression}");
    }
    assert_eq!(
        parse_default("DEFAULT q'!That's a string!'", &ColumnTypeFamily::String)
            .unwrap()
            .as_value(),
        Some(&PrismaValue::String("That's a string".to_owned()))
    );
}

#[test]
fn firebird_unsupported_column_features_fail_explicitly() {
    for (field, value, expected) in [
        ("dimensions", Value::from(1i64), "array"),
        ("is_computed", Value::from(1i64), "computed"),
        ("has_validation", Value::from(1i64), "domain CHECK"),
        ("has_default", Value::from(1i64), "default without retained SQL source"),
        ("computed_source", Value::from("A + B"), "computed"),
        ("validation_source", Value::from("CHECK (VALUE > 0)"), "domain CHECK"),
        ("identity_type", Value::from(0i64), "identity type"),
    ] {
        let mut catalog = one_column(8, 0);
        set(&mut catalog.columns[0], field, value);
        assert!(catalog.describe().unwrap_err().to_string().contains(expected));
    }
    assert!(
        one_column(999, 0)
            .describe()
            .unwrap_err()
            .to_string()
            .contains("type code 999")
    );
}

#[test]
fn firebird_unsupported_indexes_and_relations_fail_explicitly() {
    for (field, value) in [(4usize, Value::from(1i64)), (5, Value::from("UPPER(MixedCase)"))] {
        let mut catalog = one_column(8, 0);
        let mut row = index("Mapped Table", "IDX", "MixedCase", 0, None);
        row[field] = value;
        catalog.indexes.push(row);
        assert!(
            catalog
                .describe()
                .unwrap_err()
                .to_string()
                .contains("inactive or expression index")
        );
    }
    for kind in [1i64, 2, 4, 5] {
        let mut catalog = Catalog::default();
        catalog.tables.push(vec!["Special".into(), kind.into(), null()]);
        assert!(
            catalog
                .describe()
                .unwrap_err()
                .to_string()
                .contains("only persistent tables")
        );
    }
}

#[test]
fn firebird_invalid_identity_and_numeric_metadata_fail_without_panicking() {
    let mut catalog = one_column(8, 0);
    set(&mut catalog.columns[0], "identity_type", 1i64);
    set(&mut catalog.columns[0], "generator_name", "MISSING");
    assert!(
        catalog
            .describe()
            .unwrap_err()
            .to_string()
            .contains("identity generator")
    );
    let mut catalog = one_column(16, 2);
    set(&mut catalog.columns[0], "field_precision", 19i64);
    set(&mut catalog.columns[0], "field_scale", -6i64);
    assert!(
        catalog
            .describe()
            .unwrap_err()
            .to_string()
            .contains("numeric precision/scale")
    );
    let mut catalog = one_column(16, 2);
    set(&mut catalog.columns[0], "field_precision", 18i64);
    set(&mut catalog.columns[0], "field_scale", i64::MIN);
    assert!(catalog.describe().unwrap_err().to_string().contains("scale overflow"));
    let schema = build_schema(
        ResultSet::default(),
        ResultSet::default(),
        ResultSet::default(),
        ResultSet::default(),
        ResultSet::default(),
        ResultSet::default(),
        ResultSet::default(),
    )
    .unwrap();
    assert!(schema.is_empty());
}

#[test]
fn firebird_internal_migration_lock_table_is_excluded_exactly() {
    let mut catalog = Catalog::default();
    catalog.table(MIGRATION_LOCK_TABLE);
    catalog.table("_PRISMA_MIGRATE_LOCK");
    catalog.columns.push(column(MIGRATION_LOCK_TABLE, "id", 8));
    catalog.columns.push(column("_PRISMA_MIGRATE_LOCK", "id", 8));
    catalog.indexes.push(index(
        MIGRATION_LOCK_TABLE,
        "INTERNAL_PK",
        "id",
        0,
        Some(("INTERNAL_PK", "PRIMARY KEY")),
    ));
    let schema = catalog.describe().unwrap();
    assert!(schema.table_walker(MIGRATION_LOCK_TABLE).is_none());
    assert!(schema.table_walker("_PRISMA_MIGRATE_LOCK").is_some());
    assert_eq!(schema.walk_table_columns().count(), 1);
}

#[test]
fn firebird_catalog_names_outside_the_provider_identifier_range_are_explicit_errors() {
    for name in ["A".repeat(32), "\u{e9}".repeat(16), "\u{1f600}".to_owned()] {
        let mut catalog = Catalog::default();
        catalog.table(&name);
        let message = catalog.describe().unwrap_err().to_string();
        assert!(message.contains("31 UTF8 bytes"), "{message}");
    }
    assert_eq!(checked_identifier(" leadingName  ".to_owned()).unwrap(), " leadingName");
}

#[test]
fn firebird_catalog_queries_do_not_read_or_advance_generator_counters() {
    assert!(GENERATORS.contains("RDB$INITIAL_VALUE"));
    assert!(GENERATORS.contains("RDB$GENERATOR_INCREMENT"));
    for query in [RELATIONS, COLUMNS, INDEXES, FOREIGN_KEYS, CHECKS, GENERATORS, TRIGGERS] {
        assert!(!query.contains("GEN_ID("));
        assert!(!query.contains("NEXT VALUE FOR"));
    }
}
