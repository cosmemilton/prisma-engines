// Modified for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
use super::{Result, Visitor, query_writer::QueryWriter};
use crate::{
    ast::*,
    error::{Error, ErrorKind},
};
use chrono::Timelike;
use query_template::{PlaceholderFormat, QueryTemplate};
use std::{borrow::Cow, fmt};

/// SQL generation for Firebird 3.0 (SQL dialect 3), executed through a driver adapter.
pub struct Firebird<'a> {
    query_template: QueryTemplate<Value<'a>>,
}

impl<'a> Firebird<'a> {
    fn unsupported(construct: &str) -> Error {
        Error::builder(ErrorKind::conversion(format!(
            "Firebird 3.0 does not support {construct} in this connector."
        )))
        .build()
    }

    fn quoted_string(&mut self, value: impl fmt::Display) -> Result {
        self.write("'")?;
        self.write(value.to_string().replace('\'', "''"))?;
        self.write("'")
    }

    fn validate_target(table: &Table<'_>) -> Result {
        if table.database.is_some() || !matches!(table.typ, TableType::Table(_)) {
            return Err(Self::unsupported("qualified or derived DML targets"));
        }
        Ok(())
    }

    fn visit_returning(&mut self, returning: Option<Vec<Column<'a>>>) -> Result {
        if let Some(columns) = returning
            && !columns.is_empty()
        {
            self.write(" RETURNING ")?;
            for (i, mut column) in columns.into_iter().enumerate() {
                if column.name == "*" {
                    return Err(Self::unsupported("RETURNING *; explicit columns are required"));
                }
                if i > 0 {
                    self.write(", ")?;
                }
                column.table = None;
                self.visit_column(column)?;
            }
        }
        Ok(())
    }

    fn is_tuple(expression: &Expression<'_>) -> bool {
        match &expression.kind {
            ExpressionKind::Row(row) => row.len() > 1,
            ExpressionKind::Value(value) => Self::is_tuple(value),
            _ => false,
        }
    }

    fn validate_parameter(value: &Value<'_>) -> Result {
        match &value.typed {
            ValueType::Array(_) | ValueType::EnumArray(_, _) => Err(Self::unsupported("array values")),
            ValueType::Enum(_, _) => Err(Self::unsupported("enum values")),
            ValueType::Json(_) => Err(Self::unsupported("JSON values")),
            ValueType::Xml(_) => Err(Self::unsupported("XML values")),
            ValueType::Opaque(value)
                if matches!(
                    value.typ(),
                    OpaqueType::Array(_)
                        | OpaqueType::Tuple(_)
                        | OpaqueType::Json
                        | OpaqueType::Object
                        | OpaqueType::Enum
                        | OpaqueType::Xml
                ) =>
            {
                Err(Self::unsupported("array, tuple, JSON, enum or XML parameters"))
            }
            _ => Ok(()),
        }
    }
}

impl<'a> Visitor<'a> for Firebird<'a> {
    const C_BACKTICK_OPEN: &'static str = "\"";
    const C_BACKTICK_CLOSE: &'static str = "\"";
    const C_WILDCARD: &'static str = "%";

    fn build_template<Q>(query: Q) -> crate::Result<QueryTemplate<Value<'a>>>
    where
        Q: Into<Query<'a>>,
    {
        let mut visitor = Self {
            query_template: QueryTemplate::new(PlaceholderFormat {
                prefix: "?",
                has_numbering: false,
            }),
        };
        visitor.visit_query(query.into())?;
        Ok(visitor.query_template)
    }

    fn write(&mut self, value: impl fmt::Display) -> Result {
        self.query_template.write_string_chunk(value.to_string());
        Ok(())
    }

    fn add_parameter(&mut self, value: Value<'a>) {
        self.query_template.parameters.push(value);
    }

    fn parameter_substitution(&mut self) -> Result {
        self.query_template.write_parameter();
        Ok(())
    }

    fn surround_with_backticks(&mut self, identifier: &str) -> Result {
        self.write("\"")?;
        self.write(identifier.replace('"', "\"\""))?;
        self.write("\"")
    }

    fn visit_parameterized(&mut self, value: Value<'a>) -> Result {
        Self::validate_parameter(&value)?;
        self.add_parameter(value);
        self.parameter_substitution()
    }

    fn visit_parameterized_row(
        &mut self,
        value: Value<'a>,
        item_prefix: impl Into<Cow<'static, str>>,
        separator: impl Into<Cow<'static, str>>,
        item_suffix: impl Into<Cow<'static, str>>,
    ) -> Result {
        // The array here is a template expansion for IN (?, ...), not a database array.
        match &value.typed {
            ValueType::Array(Some(values)) => {
                for value in values {
                    Self::validate_parameter(value)?;
                }
            }
            ValueType::Opaque(opaque) => match opaque.typ() {
                OpaqueType::Array(inner)
                    if !matches!(
                        **inner,
                        OpaqueType::Array(_)
                            | OpaqueType::Tuple(_)
                            | OpaqueType::Json
                            | OpaqueType::Object
                            | OpaqueType::Enum
                            | OpaqueType::Xml
                    ) =>
                {
                    ()
                }
                // The compiler represents a selector list with its element type,
                // including singleton selectors, and expands the tuple at execution.
                _ => Self::validate_parameter(&value)?,
            },
            _ => Self::validate_parameter(&value)?,
        }
        self.query_template
            .write_parameter_tuple(item_prefix, separator, item_suffix);
        self.add_parameter(value);
        Ok(())
    }

    fn visit_limit_and_offset(&mut self, limit: Option<Value<'a>>, offset: Option<Value<'a>>) -> Result {
        if let Some(offset) = offset {
            self.write(" OFFSET ")?;
            self.visit_parameterized(offset)?;
            self.write(" ROWS")?;
            if let Some(limit) = limit {
                self.write(" FETCH NEXT ")?;
                self.visit_parameterized(limit)?;
                self.write(" ROWS ONLY")?;
            }
        } else if let Some(limit) = limit {
            self.write(" FETCH FIRST ")?;
            self.visit_parameterized(limit)?;
            self.write(" ROWS ONLY")?;
        }
        Ok(())
    }

    fn visit_select(&mut self, mut select: Select<'a>) -> Result {
        if matches!(select.distinct, Some(DistinctType::OnClause(_))) {
            return Err(Self::unsupported("DISTINCT ON"));
        }
        if select.tables.is_empty() {
            if select.columns.is_empty() {
                return Err(Self::unsupported("SELECT without columns or a source"));
            }
            // Firebird requires a FROM clause, including in scalar subqueries.
            select.tables.push(Table::from("RDB$DATABASE"));
        } else if select.columns.is_empty() {
            // Use the normal identifier visitor for aliases, including escaped quotes.
            select.columns = select
                .tables
                .iter()
                .cloned()
                .map(|mut table| {
                    if let Some(alias) = table.alias.take() {
                        return Table::from(alias.into_owned()).asterisk();
                    }
                    match table.typ {
                        TableType::JoinedTable(joined) => table.typ = TableType::Table(joined.0),
                        TableType::Query(_) | TableType::Values(_) => return asterisk(),
                        TableType::Table(_) => (),
                    }
                    table.asterisk()
                })
                .collect();
        }
        self.visit_select_default(select)
    }

    fn visit_table(&mut self, table: Table<'a>, include_alias: bool) -> Result {
        if table.database.is_some() {
            return Err(Self::unsupported("database or schema qualified tables"));
        }
        match table.typ {
            TableType::Table(name) => self.surround_with_backticks(&name)?,
            TableType::Query(select) => self.surround_with("(", ")", |s| s.visit_select(*select))?,
            TableType::JoinedTable(joined) => {
                self.surround_with_backticks(&joined.0)?;
                self.visit_joins(joined.1)?;
            }
            TableType::Values(_) => return Err(Self::unsupported("VALUES derived tables")),
        }
        if include_alias && let Some(alias) = table.alias {
            self.write(" AS ")?;
            self.surround_with_backticks(&alias)?;
        }
        Ok(())
    }

    fn visit_join_data(&mut self, data: JoinData<'a>) -> Result {
        if data.lateral {
            return Err(Self::unsupported("LATERAL joins"));
        }
        self.visit_table(data.table, true)?;
        self.write(" ON ")?;
        self.visit_conditions(data.conditions)
    }

    fn visit_ordering(&mut self, ordering: Ordering<'a>) -> Result {
        for (i, (expression, order)) in ordering.0.into_iter().enumerate() {
            if i > 0 {
                self.write(", ")?;
            }
            self.visit_expression(expression)?;
            self.write(match order {
                None => "",
                Some(Order::Asc) => " ASC",
                Some(Order::Desc) => " DESC",
                Some(Order::AscNullsFirst) => " ASC NULLS FIRST",
                Some(Order::AscNullsLast) => " ASC NULLS LAST",
                Some(Order::DescNullsFirst) => " DESC NULLS FIRST",
                Some(Order::DescNullsLast) => " DESC NULLS LAST",
            })?;
        }
        Ok(())
    }

    fn visit_insert(&mut self, insert: Insert<'a>) -> Result {
        if insert.on_conflict.is_some() {
            return Err(Self::unsupported("ON CONFLICT or skipDuplicates"));
        }
        let table = insert
            .table
            .ok_or_else(|| Self::unsupported("INSERT without a target"))?;
        Self::validate_target(&table)?;
        if table.alias.is_some() {
            return Err(Self::unsupported("aliased INSERT targets"));
        }
        let row = match insert.values.kind {
            ExpressionKind::Row(row) => row,
            ExpressionKind::Values(values) if values.len() == 1 => values.into_iter().next().unwrap(),
            _ => return Err(Self::unsupported("multi-row or INSERT SELECT statements")),
        };
        if row.len() != insert.columns.len() {
            return Err(Self::unsupported("INSERT with mismatched columns and values"));
        }
        self.write("INSERT INTO ")?;
        self.visit_table(table, false)?;
        if row.is_empty() {
            self.write(" DEFAULT VALUES")?;
        } else {
            self.write(" (")?;
            for (i, column) in insert.columns.into_iter().enumerate() {
                if i > 0 {
                    self.write(", ")?;
                }
                self.surround_with_backticks(&column.name)?;
            }
            self.write(") VALUES ")?;
            self.visit_row(row)?;
        }
        self.visit_returning(insert.returning)?;
        if let Some(comment) = insert.comment {
            self.write(" ")?;
            self.visit_comment(comment)?;
        }
        Ok(())
    }

    fn visit_update(&mut self, update: Update<'a>) -> Result {
        Self::validate_target(&update.table)?;
        if update.columns.is_empty() || update.columns.len() != update.values.len() {
            return Err(Self::unsupported("UPDATE without valid assignments"));
        }
        self.write("UPDATE ")?;
        self.visit_table(update.table, true)?;
        self.write(" SET ")?;
        for (i, (column, value)) in update.columns.into_iter().zip(update.values).enumerate() {
            if i > 0 {
                self.write(", ")?;
            }
            self.surround_with_backticks(&column.name)?;
            self.write(" = ")?;
            self.visit_expression(value)?;
        }
        if let Some(conditions) = update.conditions {
            self.write(" WHERE ")?;
            self.visit_conditions(conditions)?;
        }
        // DSQL RETURNING is a singleton in Firebird 3. The caller must ensure at most one row.
        self.visit_returning(update.returning)?;
        if let Some(comment) = update.comment {
            self.write(" ")?;
            self.visit_comment(comment)?;
        }
        Ok(())
    }

    fn visit_delete(&mut self, delete: Delete<'a>) -> Result {
        Self::validate_target(&delete.table)?;
        self.write("DELETE FROM ")?;
        self.visit_table(delete.table, true)?;
        if let Some(conditions) = delete.conditions {
            self.write(" WHERE ")?;
            self.visit_conditions(conditions)?;
        }
        self.visit_returning(delete.returning)?;
        if let Some(comment) = delete.comment {
            self.write(" ")?;
            self.visit_comment(comment)?;
        }
        Ok(())
    }

    fn visit_expression(&mut self, expression: Expression<'a>) -> Result {
        if matches!(expression.kind, ExpressionKind::Default) {
            return Err(Self::unsupported("DEFAULT as a value expression"));
        }
        self.visit_expression_default(expression)
    }

    fn visit_compare(&mut self, compare: Compare<'a>) -> Result {
        match &compare {
            Compare::Equals(left, right)
            | Compare::NotEquals(left, right)
            | Compare::LessThan(left, right)
            | Compare::LessThanOrEquals(left, right)
            | Compare::GreaterThan(left, right)
            | Compare::GreaterThanOrEquals(left, right)
                if Self::is_tuple(left) || Self::is_tuple(right) =>
            {
                return Err(Self::unsupported("row value comparisons"));
            }
            Compare::In(left, _) | Compare::NotIn(left, _) if Self::is_tuple(left) => {
                return Err(Self::unsupported("row value IN predicates"));
            }
            // Raw operators are supplied by the caller and cannot be checked for dialect compatibility.
            _ => (),
        }
        self.visit_compare_default(compare)
    }

    fn visit_literal_starts_with(&mut self, left: Expression<'a>, right: Expression<'a>, not: bool) -> Result {
        self.visit_expression(left)?;
        self.write(if not { " NOT STARTING WITH " } else { " STARTING WITH " })?;
        match right.kind() {
            ExpressionKind::Column(_) => self.visit_expression(right),
            ExpressionKind::Parameterized(_) => {
                // An untyped prefix inherits the searched column's width, rejecting longer filters before comparison.
                // Cast the bind itself, independently of that width, to the provider's maximum UTF8 VARCHAR size.
                self.surround_with("CAST(", " AS VARCHAR(8191) CHARACTER SET UTF8)", |s| {
                    s.visit_expression(right)
                })
            }
            _ => Err(Self::unsupported(
                "literal prefixes other than a bound string or a column reference",
            )),
        }
    }

    fn visit_multiple_tuple_comparison(&mut self, _left: Row<'a>, _right: Values<'a>, _negate: bool) -> Result {
        Err(Self::unsupported("row value IN predicates"))
    }

    fn visit_current_date(&mut self) -> Result {
        self.write("CURRENT_DATE")
    }

    fn visit_function(&mut self, function: Function<'a>) -> Result {
        if function.alias.as_ref().is_some_and(|alias| alias.len() > 31) {
            return Err(Self::unsupported(
                "generated function aliases longer than the 31-byte identifier limit",
            ));
        }
        if matches!(
            function.typ_,
            FunctionType::RowToJson(_) | FunctionType::Uuid | FunctionType::UuidToBin | FunctionType::UuidToBinSwapped
        ) {
            return Err(Self::unsupported("JSON row or UUID functions"));
        }
        self.visit_function_default(function)
    }

    fn visit_concat(&mut self, concat: Concat<'a>) -> Result {
        if concat.exprs.is_empty() {
            return self.quoted_string("");
        }
        self.surround_with("(", ")", |s| {
            for (i, expression) in concat.exprs.into_iter().enumerate() {
                if i > 0 {
                    s.write(" || ")?;
                }
                s.visit_expression(expression)?;
            }
            Ok(())
        })
    }

    fn visit_operation(&mut self, operation: SqlOp<'a>) -> Result {
        let (left, operator, right) = match operation {
            SqlOp::Add(left, right) => (left, " + ", right),
            SqlOp::Sub(left, right) => (left, " - ", right),
            SqlOp::Mul(left, right) => (left, " * ", right),
            SqlOp::Div(left, right) => (left, " / ", right),
            SqlOp::Rem(left, right) => {
                return self.surround_with("MOD(", ")", |s| {
                    s.visit_expression(left)?;
                    s.write(", ")?;
                    s.visit_expression(right)
                });
            }
        };
        self.surround_with("(", ")", |s| {
            s.visit_expression(left)?;
            s.write(operator)?;
            s.visit_expression(right)
        })
    }

    fn visit_aggregate_to_string(&mut self, value: Expression<'a>) -> Result {
        self.surround_with("LIST(", ")", |s| s.visit_expression(value))
    }

    fn visit_average(&mut self, average: Average<'a>) -> Result {
        match average.column.type_family {
            // Firebird 3 preserves the argument type for AVG. Prisma's integer
            // averages are floating-point results, so cast before aggregation.
            Some(TypeFamily::Int) => self.surround_with("AVG(CAST(", " AS DOUBLE PRECISION))", |s| {
                s.visit_column(average.column)
            }),
            Some(TypeFamily::Float | TypeFamily::Double) => {
                self.surround_with("AVG(", ")", |s| s.visit_column(average.column))
            }
            Some(TypeFamily::Decimal(_)) => Err(Self::unsupported(
                "AVG over Decimal columns; exact precision widening is not implemented",
            )),
            _ => Err(Self::unsupported("AVG without supported numeric column type metadata")),
        }
    }

    fn visit_raw_value(&mut self, value: Value<'a>) -> Result {
        Self::validate_parameter(&value)?;
        if value.is_null() {
            return self.write("NULL");
        }
        match value.typed {
            ValueType::Int32(Some(value)) => self.write(value),
            ValueType::Int64(Some(value)) => self.write(value),
            ValueType::Float(Some(value)) if value.is_finite() => self.write(format!("{value:?}")),
            ValueType::Double(Some(value)) if value.is_finite() => self.write(format!("{value:?}")),
            ValueType::Float(_) | ValueType::Double(_) => Err(Self::unsupported("non-finite numeric literals")),
            ValueType::Numeric(Some(value)) => self.write(value),
            ValueType::Boolean(Some(value)) => self.write(if value { "TRUE" } else { "FALSE" }),
            ValueType::Text(Some(value)) => self.quoted_string(value),
            ValueType::Char(Some(value)) => self.quoted_string(value),
            ValueType::Bytes(Some(value)) => self.write(format!("x'{}'", hex::encode(value))),
            ValueType::Uuid(Some(value)) => self.quoted_string(value.hyphenated()),
            ValueType::DateTime(Some(value)) => {
                self.write("TIMESTAMP ")?;
                self.quoted_string(format!(
                    "{}.{:04}",
                    value.format("%Y-%m-%d %H:%M:%S"),
                    value.nanosecond() / 100_000
                ))
            }
            ValueType::Date(Some(value)) => {
                self.write("DATE ")?;
                self.quoted_string(value)
            }
            ValueType::Time(Some(value)) => {
                self.write("TIME ")?;
                self.quoted_string(format!(
                    "{}.{:04}",
                    value.format("%H:%M:%S"),
                    value.nanosecond() / 100_000
                ))
            }
            ValueType::Opaque(value) => Err(Error::builder(ErrorKind::OpaqueAsRawValue(value.to_string())).build()),
            _ => Err(Self::unsupported("this literal type")),
        }
    }

    fn visit_merge(&mut self, _merge: Merge<'a>) -> Result {
        Err(Self::unsupported("MERGE"))
    }
    fn visit_upsert(&mut self, _update: Update<'a>) -> Result {
        Err(Self::unsupported("ON CONFLICT UPDATE"))
    }
    fn visit_json_extract(&mut self, _extract: JsonExtract<'a>) -> Result {
        Err(Self::unsupported("JSON extraction"))
    }
    fn visit_json_extract_last_array_item(&mut self, _extract: JsonExtractLastArrayElem<'a>) -> Result {
        Err(Self::unsupported("JSON extraction"))
    }
    fn visit_json_extract_first_array_item(&mut self, _extract: JsonExtractFirstArrayElem<'a>) -> Result {
        Err(Self::unsupported("JSON extraction"))
    }
    fn visit_json_array_contains(&mut self, _left: Expression<'a>, _right: Expression<'a>, _not: bool) -> Result {
        Err(Self::unsupported("JSON predicates"))
    }
    fn visit_json_type_equals(&mut self, _left: Expression<'a>, _right: JsonType<'a>, _not: bool) -> Result {
        Err(Self::unsupported("JSON predicates"))
    }
    fn visit_json_unquote(&mut self, _value: JsonUnquote<'a>) -> Result {
        Err(Self::unsupported("JSON unquoting"))
    }
    fn visit_json_array_agg(&mut self, _value: JsonArrayAgg<'a>) -> Result {
        Err(Self::unsupported("JSON aggregation"))
    }
    fn visit_json_build_object(&mut self, _value: JsonBuildObject<'a>) -> Result {
        Err(Self::unsupported("JSON objects"))
    }
    fn visit_stringify(&mut self, _value: Stringify<'a>) -> Result {
        Err(Self::unsupported("string conversion"))
    }
    fn visit_text_search(&mut self, _value: TextSearch<'a>) -> Result {
        Err(Self::unsupported("full-text search"))
    }
    fn visit_matches(&mut self, _left: Expression<'a>, _right: Expression<'a>, _not: bool) -> Result {
        Err(Self::unsupported("full-text search"))
    }
    fn visit_text_search_relevance(&mut self, _value: TextSearchRelevance<'a>) -> Result {
        Err(Self::unsupported("full-text search"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_unsupported(query: impl Into<Query<'static>>, construct: &str) {
        let error = Firebird::build(query).unwrap_err().to_string();
        assert!(error.contains("Firebird 3.0"), "{error}");
        assert!(error.contains(construct), "{error}");
    }

    #[test]
    fn select_filters_joins_and_pagination_keep_sql_parameter_order() {
        let join = "Post".on(Column::from(("Post", "authorId"))
            .equals(Column::from(("User", "id")))
            .and(Column::from(("Post", "published")).equals(true)));
        let query = Select::from_table("User")
            .column(("User", "id"))
            .left_join(join)
            .so_that(
                Column::from(("User", "name"))
                    .like("O'%")
                    .and(Column::from(("User", "age")).greater_than(18)),
            )
            .order_by(Column::from(("User", "name")).ascend_nulls_last())
            .offset(7)
            .limit(3);
        let (sql, params) = Firebird::build(query).unwrap();
        assert_eq!(
            sql,
            "SELECT \"User\".\"id\" FROM \"User\" LEFT JOIN \"Post\" ON (\"Post\".\"authorId\" = \"User\".\"id\" AND \"Post\".\"published\" = ?) WHERE (\"User\".\"name\" LIKE ? AND \"User\".\"age\" > ?) ORDER BY \"User\".\"name\" ASC NULLS LAST OFFSET ? ROWS FETCH NEXT ? ROWS ONLY"
        );
        assert_eq!(
            params,
            vec![
                Value::from(true),
                Value::from("O'%"),
                Value::from(18),
                Value::from(7_usize),
                Value::from(3_usize)
            ]
        );
    }

    #[test]
    fn limit_and_offset_are_independently_supported() {
        let (sql, params) = Firebird::build(Select::from_table("Item").limit(0)).unwrap();
        assert_eq!(sql, "SELECT \"Item\".* FROM \"Item\" FETCH FIRST ? ROWS ONLY");
        assert_eq!(params, vec![Value::from(0_usize)]);
        let (sql, params) = Firebird::build(Select::from_table("Item").offset(10)).unwrap();
        assert_eq!(sql, "SELECT \"Item\".* FROM \"Item\" OFFSET ? ROWS");
        assert_eq!(params, vec![Value::from(10_usize)]);
    }

    #[test]
    fn identifiers_and_aliases_escape_embedded_quotes() {
        let query = Select::from_table(Table::from("my\"table").alias("u\"x"))
            .column(Column::from(("u\"x", "a\"b")).alias("c\"d"));
        let (sql, params) = Firebird::build(query).unwrap();
        assert_eq!(
            sql,
            "SELECT \"u\"\"x\".\"a\"\"b\" AS \"c\"\"d\" FROM \"my\"\"table\" AS \"u\"\"x\""
        );
        assert!(params.is_empty());
        let (sql, _) = Firebird::build(Select::from_table(Table::from("Item").alias("i\"x"))).unwrap();
        assert_eq!(sql, "SELECT \"i\"\"x\".* FROM \"Item\" AS \"i\"\"x\"");
    }

    #[test]
    fn scalar_select_uses_the_single_row_system_table() {
        let (sql, params) = Firebird::build(Select::default().value(42).so_that(ConditionTree::NoCondition)).unwrap();
        assert_eq!(sql, "SELECT ? FROM \"RDB$DATABASE\" WHERE 1=1");
        assert_eq!(params, vec![Value::from(42)]);
    }

    #[test]
    fn insert_returns_explicit_unqualified_columns() {
        let query: Insert = Insert::single_into("User")
            .value("name", "O'Brien")
            .value("active", true)
            .into();
        let query = query.returning([
            Column::from(("User", "id")),
            Column::from(("User", "name")).alias("displayName"),
        ]);
        let (sql, params) = Firebird::build(query).unwrap();
        assert_eq!(
            sql,
            "INSERT INTO \"User\" (\"name\", \"active\") VALUES (?,?) RETURNING \"id\", \"name\" AS \"displayName\""
        );
        assert_eq!(params, vec![Value::from("O'Brien"), Value::from(true)]);
    }

    #[test]
    fn default_values_insert_can_return_generated_identity() {
        let query: Insert = Insert::single_into("User").into();
        let (sql, params) = Firebird::build(query.returning(["id"])).unwrap();
        assert_eq!(sql, "INSERT INTO \"User\" DEFAULT VALUES RETURNING \"id\"");
        assert!(params.is_empty());
    }

    #[test]
    fn update_assignments_and_returning_are_unqualified() {
        let query = Update::table("User")
            .set(Column::from(("User", "name")), "renamed")
            .set("score", Expression::from(Column::from("score")) + Expression::from(2))
            .so_that(Column::from("id").equals(9))
            .returning([Column::from(("User", "id"))]);
        let (sql, params) = Firebird::build(query).unwrap();
        assert_eq!(
            sql,
            "UPDATE \"User\" SET \"name\" = ?, \"score\" = (\"score\" + ?) WHERE \"id\" = ? RETURNING \"id\""
        );
        assert_eq!(params, vec![Value::from("renamed"), Value::from(2), Value::from(9)]);
    }

    #[test]
    fn current_date_is_inline_in_insert_and_update_without_consuming_parameters() {
        let insert: Insert = Insert::single_into("DATE_EXPR_PROBE")
            .value("CREATED_DATE", current_date())
            .value("STATUS", "1")
            .into();
        let (sql, params) = Firebird::build(insert.returning(["ID", "CREATED_DATE"])).unwrap();
        assert_eq!(
            sql,
            "INSERT INTO \"DATE_EXPR_PROBE\" (\"CREATED_DATE\", \"STATUS\") VALUES (CURRENT_DATE,?) RETURNING \"ID\", \"CREATED_DATE\""
        );
        assert_eq!(params, vec![Value::from("1")]);

        let update = Update::table("DATE_EXPR_PROBE")
            .set("USED_DATE", current_date())
            .set("STATUS", "2")
            .so_that(Column::from("ID").equals(7))
            .returning(["ID", "USED_DATE"]);
        let (sql, params) = Firebird::build(update).unwrap();
        assert_eq!(
            sql,
            "UPDATE \"DATE_EXPR_PROBE\" SET \"USED_DATE\" = CURRENT_DATE, \"STATUS\" = ? WHERE \"ID\" = ? RETURNING \"ID\", \"USED_DATE\""
        );
        assert_eq!(params, vec![Value::from("2"), Value::from(7)]);
    }

    #[test]
    fn ordinary_current_date_string_remains_a_bound_value() {
        let insert: Insert = Insert::single_into("DATE_EXPR_PROBE")
            .value("CREATED_DATE", "CURRENT_DATE")
            .into();
        let (sql, params) = Firebird::build(insert).unwrap();
        assert_eq!(sql, "INSERT INTO \"DATE_EXPR_PROBE\" (\"CREATED_DATE\") VALUES (?)");
        assert_eq!(params, vec![Value::from("CURRENT_DATE")]);
    }

    #[test]
    #[cfg(feature = "mysql")]
    fn a_different_sql_visitor_rejects_current_date() {
        let error = crate::visitor::Mysql::build(Select::default().value(current_date()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("CURRENT_DATE is not supported"), "{error}");
    }

    #[test]
    fn delete_preserves_conditions_and_explicit_returning() {
        let query = Delete::from_table("User")
            .so_that(Column::from("id").equals(9))
            .returning(["id"]);
        let (sql, params) = Firebird::build(query).unwrap();
        assert_eq!(sql, "DELETE FROM \"User\" WHERE \"id\" = ? RETURNING \"id\"");
        assert_eq!(params, vec![Value::from(9)]);
    }

    #[test]
    fn raw_literals_escape_strings_and_use_boolean_keywords() {
        let query = Select::default()
            .value("O'Brien".raw())
            .value(true.raw())
            .value(false.raw())
            .value(Value::bytes(vec![0, 255]).raw())
            .value(Value::null_int32().raw());
        let (sql, params) = Firebird::build(query).unwrap();
        assert_eq!(
            sql,
            "SELECT 'O''Brien', TRUE, FALSE, x'00ff', NULL FROM \"RDB$DATABASE\""
        );
        assert!(params.is_empty());
    }

    #[test]
    fn scalar_in_lists_support_static_and_template_parameters() {
        let query = Select::from_table("User").so_that(Column::from("id").in_selection(vec![1, 2]));
        let (sql, params) = Firebird::build(query).unwrap();
        assert_eq!(sql, "SELECT \"User\".* FROM \"User\" WHERE \"id\" IN (?,?)");
        assert_eq!(params, vec![Value::from(1), Value::from(2)]);
        let list = Value::array(vec![Value::from(1), Value::from(2)]);
        let row = Row::from(vec![list.clone()]).to_parameterized_row();
        let template =
            Firebird::build_template(Select::from_table("User").so_that(Column::from("id").in_selection(row))).unwrap();
        assert_eq!(template.parameters, vec![list]);
        assert!(
            matches!(template.fragments.first(), Some(query_template::Fragment::StringChunk { chunk })
            if chunk == "SELECT \"User\".* FROM \"User\" WHERE \"id\" IN ")
        );
        assert!(matches!(
            template.fragments.last(),
            Some(query_template::Fragment::ParameterTuple { .. })
        ));
    }

    #[test]
    fn compiler_singleton_selector_placeholders_keep_tuple_expansion() {
        let selector: Value = Value::opaque("selector-id", OpaqueType::Int32);
        let row = Row::from(vec![selector.clone()]).to_parameterized_row();
        let template = Firebird::build_template(
            Select::from_table("User").so_that(Row::from(vec![Column::from("id")]).in_selection(row)),
        )
        .unwrap();
        assert_eq!(template.parameters, vec![selector]);
        assert!(
            matches!(template.fragments.first(), Some(query_template::Fragment::StringChunk { chunk })
            if chunk == "SELECT \"User\".* FROM \"User\" WHERE \"id\" IN ")
        );
        assert!(matches!(
            template.fragments.last(),
            Some(query_template::Fragment::ParameterTuple { .. })
        ));
    }

    #[test]
    fn rejects_conflicts_bulk_insert_and_wildcard_returning() {
        let query: Insert = Insert::single_into("User").value("id", 1).into();
        assert_unsupported(query.clone().on_conflict(OnConflict::DoNothing), "ON CONFLICT");
        assert_unsupported(query.returning(["*"]), "explicit columns");
        assert_unsupported(
            Insert::multi_into("User", ["id"]).values(vec![1]).values(vec![2]),
            "multi-row",
        );
        let query = Insert::expression_into("User", ["id"], Select::default().value(1));
        assert_unsupported(query, "INSERT SELECT");
    }

    #[test]
    fn rejects_json_arrays_distinct_on_lateral_and_schema_qualification() {
        assert_unsupported(
            Select::from_table("User").distinct_on(vec![Column::from("id").into()]),
            "DISTINCT ON",
        );
        assert_unsupported(
            Select::from_table("User").inner_join("Post".on(ConditionTree::NoCondition).lateral()),
            "LATERAL",
        );
        assert_unsupported(Select::from_table(("public", "User")), "schema qualified");
        assert_unsupported(
            Select::default().value(json_extract(Column::from("data"), JsonPath::string("$.a"), false)),
            "JSON extraction",
        );
        assert_unsupported(Select::default().value(Value::array(vec![1, 2])), "array values");
        assert_unsupported(Select::default().value(f64::NAN.raw()), "non-finite");
    }

    #[test]
    fn rejects_row_value_comparisons_instead_of_emitting_invalid_firebird_sql() {
        let left = Row::from(vec![Column::from("id"), Column::from("tenant")]);
        assert_unsupported(
            Select::from_table("User").so_that(left.clone().equals(Row::from(vec![1, 2]))),
            "row value",
        );
        assert_unsupported(
            Select::from_table("User").so_that(left.in_selection(crate::values!((1, 2), (3, 4)))),
            "row value",
        );
    }

    #[test]
    fn adapter_provider_remains_a_distinct_firebird_family() {
        use crate::connector::{AdapterProvider, SqlFamily};
        use std::str::FromStr;
        let provider = AdapterProvider::from_str("firebird").unwrap();
        assert_eq!(provider, AdapterProvider::Firebird);
        assert_eq!(provider.db_system_name(), "firebird");
        assert_eq!(SqlFamily::from(&provider), SqlFamily::Firebird);
    }

    #[test]
    fn integer_and_bigint_averages_cast_before_aggregation() {
        for native_type in ["INTEGER", "BIGINT"] {
            let column = Column::from(("Sample", "value"))
                .type_family(TypeFamily::Int)
                .native_column_type(Some(native_type));
            let (sql, parameters) = Firebird::build(Select::from_table("Sample").value(avg(column))).unwrap();
            assert_eq!(
                sql,
                "SELECT AVG(CAST(\"Sample\".\"value\" AS DOUBLE PRECISION)) FROM \"Sample\""
            );
            assert!(parameters.is_empty());
        }
    }

    #[test]
    fn floating_point_averages_keep_their_numeric_argument() {
        for family in [TypeFamily::Float, TypeFamily::Double] {
            let column = Column::from(("Sample", "value")).type_family(family);
            let (sql, parameters) = Firebird::build(Select::from_table("Sample").value(avg(column))).unwrap();
            assert_eq!(sql, "SELECT AVG(\"Sample\".\"value\") FROM \"Sample\"");
            assert!(parameters.is_empty());
        }
    }

    #[test]
    fn decimal_and_untyped_averages_fail_explicitly() {
        for precision_scale in [None, Some((18, 2)), Some((18, 18))] {
            let column = Column::from("amount").type_family(TypeFamily::Decimal(precision_scale));
            assert_unsupported(Select::from_table("Sample").value(avg(column)), "AVG over Decimal");
        }
        assert_unsupported(
            Select::from_table("Sample").value(avg("value")),
            "numeric column type metadata",
        );
    }

    #[test]
    fn integer_average_cast_is_also_used_in_having_conditions() {
        let column = Column::from(("Sample", "value")).type_family(TypeFamily::Int);
        let average: Expression = avg(column).into();
        let query = Select::from_table("Sample")
            .value(average.clone())
            .having(average.greater_than(1.5));
        let (sql, parameters) = Firebird::build(query).unwrap();
        assert_eq!(
            sql,
            "SELECT AVG(CAST(\"Sample\".\"value\" AS DOUBLE PRECISION)) FROM \"Sample\" HAVING AVG(CAST(\"Sample\".\"value\" AS DOUBLE PRECISION)) > ?"
        );
        assert_eq!(parameters, vec![Value::from(1.5)]);
    }

    #[test]
    fn generated_function_aliases_respect_the_31_byte_limit() {
        let accepted = "a".repeat(31);
        let (sql, _) =
            Firebird::build(Select::from_table("Sample").value(count(asterisk()).alias(accepted.clone()))).unwrap();
        assert_eq!(sql, format!("SELECT COUNT(*) AS \"{accepted}\" FROM \"Sample\""));
        for rejected in ["a".repeat(32), "\u{e9}".repeat(16)] {
            assert_unsupported(
                Select::from_table("Sample").value(count(asterisk()).alias(rejected)),
                "function aliases longer than the 31-byte",
            );
        }
    }

    #[test]
    fn raw_timestamp_and_time_literals_format_four_fractional_digits_without_panicking() {
        let timestamp = chrono::DateTime::parse_from_rfc3339("2024-03-04T05:06:07.123456Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let time = chrono::NaiveTime::from_hms_nano_opt(8, 9, 10, 987_654_321).unwrap();
        let query = Select::default()
            .value(Value::datetime(timestamp).raw())
            .value(Value::time(time).raw());
        let (sql, parameters) = Firebird::build(query).unwrap();
        assert_eq!(
            sql,
            "SELECT TIMESTAMP '2024-03-04 05:06:07.1234', TIME '08:09:10.9876' FROM \"RDB$DATABASE\""
        );
        assert!(parameters.is_empty());
    }

    #[test]
    fn literal_prefix_parameters_use_a_direct_wide_cast_and_preserve_every_character() {
        let prefixes = [
            "%".to_owned(),
            "_".to_owned(),
            "A%_\\'".to_owned(),
            String::new(),
            "\u{00e9}\u{1f600}".to_owned(),
            "x".repeat(8191),
            format!("{} ", "x".repeat(8191)),
        ];
        for prefix in prefixes {
            let query = Select::from_table("IBPT")
                .column("COD_NCM")
                .so_that(Compare::LiteralStartsWith {
                    left: Box::new(Column::from(("IBPT", "COD_NCM")).into()),
                    right: Box::new(Value::text(prefix.clone()).into()),
                    not: false,
                });
            let (sql, parameters) = Firebird::build(query).unwrap();
            assert_eq!(
                sql,
                "SELECT \"COD_NCM\" FROM \"IBPT\" WHERE \"IBPT\".\"COD_NCM\" STARTING WITH CAST(? AS VARCHAR(8191) CHARACTER SET UTF8)"
            );
            assert_eq!(parameters, vec![Value::text(prefix)]);
        }
    }

    #[test]
    fn negated_literal_prefix_keeps_native_negation_and_bound_input() {
        let query = Select::from_table("IBPT").so_that(Compare::LiteralStartsWith {
            left: Box::new(Column::from("COD_NCM").into()),
            right: Box::new(Value::text("A%_").into()),
            not: true,
        });
        let (sql, parameters) = Firebird::build(query).unwrap();
        assert_eq!(
            sql,
            "SELECT \"IBPT\".* FROM \"IBPT\" WHERE \"COD_NCM\" NOT STARTING WITH CAST(? AS VARCHAR(8191) CHARACTER SET UTF8)"
        );
        assert_eq!(parameters, vec![Value::text("A%_")]);
    }

    #[test]
    fn literal_prefix_column_references_keep_their_own_type_and_quoted_aliases() {
        let query = Select::from_table(Table::from("CIDADES").alias("c\"x")).so_that(Compare::LiteralStartsWith {
            left: Box::new(Column::from(("c\"x", "NOME_MUNICIPIO")).into()),
            right: Box::new(Column::from(("c\"x", "ESTADO")).into()),
            not: false,
        });
        let (sql, parameters) = Firebird::build(query).unwrap();
        assert_eq!(
            sql,
            "SELECT \"c\"\"x\".* FROM \"CIDADES\" AS \"c\"\"x\" WHERE \"c\"\"x\".\"NOME_MUNICIPIO\" STARTING WITH \"c\"\"x\".\"ESTADO\""
        );
        assert!(parameters.is_empty());
    }

    #[test]
    fn literal_prefix_rejects_a_concatenated_expression_instead_of_casting_its_result() {
        let prefix = concat::<Expression<'_>>(vec![Value::text("prefix").into(), Value::text("%").raw().into()]);
        let query = Select::from_table("IBPT").so_that(Compare::LiteralStartsWith {
            left: Box::new(Column::from("COD_NCM").into()),
            right: Box::new(prefix.into()),
            not: false,
        });
        assert_unsupported(
            query,
            "literal prefixes other than a bound string or a column reference",
        );
    }

    #[test]
    fn ordinary_like_and_not_like_keep_their_pattern_arguments_without_a_prefix_cast() {
        let query = Select::from_table("IBPT").so_that(
            Column::from("COD_NCM")
                .like("A%_")
                .and(Column::from("COD_NCM").not_like("\\_%")),
        );
        let (sql, parameters) = Firebird::build(query).unwrap();
        assert_eq!(
            sql,
            "SELECT \"IBPT\".* FROM \"IBPT\" WHERE (\"COD_NCM\" LIKE ? AND \"COD_NCM\" NOT LIKE ?)"
        );
        assert_eq!(parameters, vec![Value::text("A%_"), Value::text("\\_%")]);
    }

    #[test]
    #[cfg(feature = "mysql")]
    fn a_different_sql_visitor_rejects_the_new_explicit_literal_prefix_node() {
        let query = Select::from_table("Item").so_that(Compare::LiteralStartsWith {
            left: Box::new(Column::from("code").into()),
            right: Box::new(Value::text("A%").into()),
            not: false,
        });
        let error = crate::visitor::Mysql::build(query).unwrap_err().to_string();
        assert!(error.contains("Literal prefix comparison is not supported"), "{error}");
    }
}
