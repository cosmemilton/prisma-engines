// Added for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
//! Conservative Firebird 3 DDL. Unsupported changes are rejected before rendering.
//! SQL reference: https://www.firebirdsql.org/file/documentation/html/en/refdocs/fblangref30/firebird-30-language-reference.html
use super::FirebirdDialect;
use crate::{
    migration_pair::MigrationPair,
    sql_migration::{AlterEnum, AlterTable, RedefineTable, SqlMigration, SqlMigrationStep, TableChange},
    sql_renderer::{Quoted, QuotedWithPrefix, SqlRenderer},
};
use psl::builtin_connectors::FirebirdType;
use schema_connector::{ConnectorError, ConnectorResult};
use sql_schema_describer::{
    DefaultKind, ForeignKeyAction, PrismaValue, SQLSortOrder, SqlSchema,
    firebird::{FirebirdSchemaExt, is_supported_default_expression},
    walkers::{
        EnumWalker, ForeignKeyWalker, IndexWalker, TableColumnWalker, TableWalker, UserDefinedTypeWalker, ViewWalker,
    },
};

fn unsupported(message: impl AsRef<str>) -> ConnectorError {
    ConnectorError::from_msg(format!("Unsupported Firebird 3 migration: {}", message.as_ref()))
}

pub(super) fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
fn quote_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn validate_identifier(name: &str) -> ConnectorResult<()> {
    if name.is_empty() || name.len() > 31 || name.ends_with(' ') || name.contains('\0') {
        return Err(unsupported(format!(
            "identifier {name:?} must contain 1 to 31 UTF8 bytes, without NUL or trailing spaces"
        )));
    }
    Ok(())
}

/// Check structure without requiring every existing column to have a supported Prisma type.
pub(super) fn validate_firebird_schema(schema: &SqlSchema) -> ConnectorResult<()> {
    if schema.enum_walkers().next().is_some()
        || schema.udt_walkers().next().is_some()
        || schema.view_walkers().next().is_some()
    {
        return Err(unsupported("enums, user-defined types, and views are not supported"));
    }
    for table in schema.table_walkers() {
        validate_identifier(table.name())?;
        if table.namespace().is_some_and(|name| !name.is_empty()) {
            return Err(unsupported("database namespaces are not supported"));
        }
        for column in table.columns() {
            validate_identifier(column.name())?;
        }
        for index in table.indexes() {
            validate_index(index)?;
        }
        for fk in table.foreign_keys() {
            validate_identifier(
                fk.constraint_name()
                    .ok_or_else(|| unsupported("unnamed foreign keys"))?,
            )?;
        }
    }
    Ok(())
}

fn validate_index(index: IndexWalker<'_>) -> ConnectorResult<()> {
    validate_identifier(index.name())?;
    if (!index.is_primary_key() && !index.is_unique() && !index.is_normal()) || index.predicate().is_some() {
        return Err(unsupported(format!(
            "index {:?}: fulltext and partial indexes are not supported",
            index.name()
        )));
    }
    let mut sort = None;
    if index.columns().len() == 0 || index.columns().len() > 16 {
        return Err(unsupported("indexes require between 1 and 16 columns"));
    }
    for column in index.columns() {
        if matches!(
            column.as_column().column_native_type::<FirebirdType>(),
            Some(FirebirdType::Text | FirebirdType::Blob)
        ) {
            return Err(unsupported("BLOB columns cannot be indexed"));
        }
        if column.length().is_some() {
            return Err(unsupported("index prefix lengths are not supported"));
        }
        let current = column.sort_order().unwrap_or_default();
        if sort.is_some_and(|previous| previous != current) {
            return Err(unsupported("mixed ASC/DESC index columns are not supported"));
        }
        sort = Some(current);
    }
    Ok(())
}

pub(super) fn native_type_sql(native: &FirebirdType) -> ConnectorResult<String> {
    use FirebirdType::*;
    Ok(match native {
        SmallInt => "SMALLINT".into(),
        Integer => "INTEGER".into(),
        BigInt => "BIGINT".into(),
        Float => "FLOAT".into(),
        DoublePrecision => "DOUBLE PRECISION".into(),
        Boolean => "BOOLEAN".into(),
        Date => "DATE".into(),
        Time => "TIME".into(),
        Timestamp => "TIMESTAMP".into(),
        Char(length) | VarChar(length) => {
            if !(1..=8191).contains(length) {
                return Err(unsupported("UTF8 CHAR/VARCHAR length must be between 1 and 8191"));
            }
            format!(
                "{}({length}) CHARACTER SET UTF8",
                if matches!(native, Char(_)) { "CHAR" } else { "VARCHAR" }
            )
        }
        Decimal(params) | Numeric(params) => {
            let (precision, scale) = params.unwrap_or((9, 0));
            if !(1..=18).contains(&precision) || scale > precision {
                return Err(unsupported(
                    "NUMERIC/DECIMAL require precision 1..18 and scale 0..precision",
                ));
            }
            format!(
                "{}({precision},{scale})",
                if matches!(native, Decimal(_)) {
                    "DECIMAL"
                } else {
                    "NUMERIC"
                }
            )
        }
        Text => "BLOB SUB_TYPE TEXT CHARACTER SET UTF8".into(),
        Blob => "BLOB SUB_TYPE BINARY".into(),
    })
}

fn column_type_sql(column: TableColumnWalker<'_>) -> ConnectorResult<String> {
    if column.arity().is_list() {
        return Err(unsupported("array/list columns"));
    }
    let native = column.column_native_type::<FirebirdType>().ok_or_else(|| {
        unsupported(format!(
            "column {:?} has no supported Firebird native type",
            column.name()
        ))
    })?;
    native_type_sql(native)
}

pub(super) fn default_sql(column: TableColumnWalker<'_>) -> ConnectorResult<Option<String>> {
    let Some(default) = column.default() else {
        return Ok(None);
    };
    let native = column.column_native_type::<FirebirdType>();
    let sql = match default.kind() {
        DefaultKind::Now => match native {
            Some(FirebirdType::Date) => "CURRENT_DATE".into(),
            Some(FirebirdType::Time) => "CURRENT_TIME".into(),
            Some(FirebirdType::Timestamp) => "CURRENT_TIMESTAMP".into(),
            _ => return Err(unsupported("now() requires a temporal column")),
        },
        DefaultKind::DbGenerated(Some(expression)) if is_supported_default_expression(expression) => {
            expression.trim().to_owned()
        }
        DefaultKind::Value(value) => match value {
            PrismaValue::String(value) => quote_string(value),
            PrismaValue::Int(value) | PrismaValue::BigInt(value) => value.to_string(),
            PrismaValue::Float(value) => value.to_string(),
            PrismaValue::Boolean(value) => {
                if *value {
                    "TRUE".into()
                } else {
                    "FALSE".into()
                }
            }
            PrismaValue::Null => "NULL".into(),
            PrismaValue::Bytes(bytes) => {
                let mut hex = String::with_capacity(bytes.len() * 2);
                for byte in bytes {
                    use std::fmt::Write;
                    write!(&mut hex, "{byte:02X}").unwrap();
                }
                format!("X'{hex}'")
            }
            PrismaValue::DateTime(value) => {
                if matches!(native, Some(FirebirdType::Time | FirebirdType::Timestamp))
                    && value.timestamp_subsec_nanos() % 100_000 != 0
                {
                    return Err(unsupported(
                        "temporal defaults require Firebird's 100-microsecond precision or coarser",
                    ));
                }
                match native {
                    Some(FirebirdType::Date) => format!("DATE '{}'", value.naive_utc().format("%Y-%m-%d")),
                    Some(FirebirdType::Time) => format!(
                        "TIME '{}.{:04}'",
                        value.naive_utc().format("%H:%M:%S"),
                        value.timestamp_subsec_micros() / 100
                    ),
                    Some(FirebirdType::Timestamp) => {
                        format!(
                            "TIMESTAMP '{}.{:04}'",
                            value.naive_utc().format("%Y-%m-%d %H:%M:%S"),
                            value.timestamp_subsec_micros() / 100
                        )
                    }
                    _ => return Err(unsupported("date/time default requires a temporal column")),
                }
            }
            _ => return Err(unsupported("this constant default value is not supported")),
        },
        _ => {
            return Err(unsupported(
                "dbgenerated defaults require an explicit Firebird 3 literal or context variable; sequence/unique_rowid defaults are not supported",
            ));
        }
    };
    Ok(Some(sql))
}

fn validate_column(column: TableColumnWalker<'_>) -> ConnectorResult<()> {
    column_type_sql(column)?;
    default_sql(column)?;
    if column.is_autoincrement() {
        if !matches!(
            column.column_native_type::<FirebirdType>(),
            Some(FirebirdType::SmallInt | FirebirdType::Integer | FirebirdType::BigInt)
        ) || column.default().is_some()
            || !column.arity().is_required()
        {
            return Err(unsupported(
                "identity requires a required integer column without another DEFAULT",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        sql_migration::{AlterColumn, ColumnTypeChange},
        sql_schema_differ::{SqlSchemaDifferFlavour, column::all_changes},
    };
    use psl::datamodel_connector::NativeTypeInstance;
    use sql_schema_describer::{
        Column, ColumnArity, ColumnType, ColumnTypeFamily, DefaultValue, IndexColumn, IndexId, NamespaceId,
        TableColumnId, TableId,
        firebird::{ColumnMetadata, IndexMetadata},
    };

    fn empty() -> SqlSchema {
        let mut schema = SqlSchema::default();
        schema.set_connector_data(Box::new(FirebirdSchemaExt::default()));
        schema
    }
    fn column(
        schema: &mut SqlSchema,
        table: TableId,
        name: &str,
        native: FirebirdType,
        required: bool,
        identity: bool,
    ) -> TableColumnId {
        let family = match native {
            FirebirdType::VarChar(_) | FirebirdType::Char(_) | FirebirdType::Text => ColumnTypeFamily::String,
            FirebirdType::Decimal(_) | FirebirdType::Numeric(_) => ColumnTypeFamily::Decimal,
            FirebirdType::BigInt => ColumnTypeFamily::BigInt,
            FirebirdType::Boolean => ColumnTypeFamily::Boolean,
            _ => ColumnTypeFamily::Int,
        };
        schema.push_table_column(
            table,
            Column {
                name: name.into(),
                tpe: ColumnType {
                    full_data_type: String::new(),
                    family,
                    arity: if required {
                        ColumnArity::Required
                    } else {
                        ColumnArity::Nullable
                    },
                    native_type: Some(NativeTypeInstance::new(native)),
                },
                auto_increment: identity,
                description: None,
            },
        )
    }
    fn index(
        schema: &mut SqlSchema,
        table: TableId,
        name: &str,
        columns: &[(TableColumnId, Option<SQLSortOrder>)],
        kind: &str,
    ) -> IndexId {
        let id = match kind {
            "pk" => schema.push_primary_key(table, name.into()),
            "unique" | "standalone" => schema.push_unique_constraint(table, name.into()),
            _ => schema.push_index(table, name.into()),
        };
        for (column_id, sort_order) in columns {
            schema.push_index_column(IndexColumn {
                index_id: id,
                column_id: *column_id,
                sort_order: *sort_order,
                length: None,
            });
        }
        let mut ext = schema.downcast_connector_data::<FirebirdSchemaExt>().clone();
        ext.indexes.push((
            id,
            IndexMetadata {
                database_name: name.into(),
                constraint_name: matches!(kind, "pk" | "unique").then(|| name.into()),
            },
        ));
        schema.set_connector_data(Box::new(ext));
        id
    }
    fn migration(before: SqlSchema, after: SqlSchema, steps: Vec<SqlMigrationStep>) -> SqlMigration {
        SqlMigration { before, after, steps }
    }

    #[test]
    fn firebird_native_types_and_conservative_limits() {
        let cases = [
            (FirebirdType::SmallInt, "SMALLINT"),
            (FirebirdType::Integer, "INTEGER"),
            (FirebirdType::BigInt, "BIGINT"),
            (FirebirdType::Float, "FLOAT"),
            (FirebirdType::DoublePrecision, "DOUBLE PRECISION"),
            (FirebirdType::Boolean, "BOOLEAN"),
            (FirebirdType::Date, "DATE"),
            (FirebirdType::Time, "TIME"),
            (FirebirdType::Timestamp, "TIMESTAMP"),
            (FirebirdType::VarChar(191), "VARCHAR(191) CHARACTER SET UTF8"),
            (FirebirdType::Char(2), "CHAR(2) CHARACTER SET UTF8"),
            (FirebirdType::Text, "BLOB SUB_TYPE TEXT CHARACTER SET UTF8"),
            (FirebirdType::Blob, "BLOB SUB_TYPE BINARY"),
            (FirebirdType::Decimal(Some((18, 6))), "DECIMAL(18,6)"),
            (FirebirdType::Numeric(None), "NUMERIC(9,0)"),
        ];
        for (native, expected) in cases {
            assert_eq!(native_type_sql(&native).unwrap(), expected);
        }
        for native in [
            FirebirdType::Numeric(Some((19, 0))),
            FirebirdType::Decimal(Some((3, 4))),
            FirebirdType::VarChar(8192),
            FirebirdType::Char(0),
        ] {
            assert!(native_type_sql(&native).is_err());
        }
        assert!(validate_identifier(&"é".repeat(16)).is_err());
        assert!(validate_identifier(&"x".repeat(31)).is_ok());
    }

    #[test]
    fn firebird_temporal_literal_defaults_render_four_fractional_digits_without_chrono_panic() {
        let mut schema = empty();
        let table = schema.push_table("A".into(), NamespaceId::default(), None);
        let time = column(&mut schema, table, "time", FirebirdType::Time, false, false);
        let timestamp = column(&mut schema, table, "stamp", FirebirdType::Timestamp, false, false);
        let date = column(&mut schema, table, "date", FirebirdType::Date, false, false);
        let value = chrono::DateTime::parse_from_rfc3339("2020-02-29T12:34:56.1234+02:00").unwrap();
        schema.push_table_default_value(time, DefaultValue::value(PrismaValue::DateTime(value)));
        schema.push_table_default_value(timestamp, DefaultValue::value(PrismaValue::DateTime(value)));
        schema.push_table_default_value(date, DefaultValue::value(PrismaValue::DateTime(value)));
        assert_eq!(
            default_sql(schema.walk(time)).unwrap().as_deref(),
            Some("TIME '10:34:56.1234'")
        );
        assert_eq!(
            default_sql(schema.walk(timestamp)).unwrap().as_deref(),
            Some("TIMESTAMP '2020-02-29 10:34:56.1234'")
        );
        assert_eq!(
            default_sql(schema.walk(date)).unwrap().as_deref(),
            Some("DATE '2020-02-29'")
        );
    }

    #[test]
    fn firebird_temporal_literal_default_rejects_unrepresentable_fraction() {
        let mut schema = empty();
        let table = schema.push_table("A".into(), NamespaceId::default(), None);
        let field = column(&mut schema, table, "stamp", FirebirdType::Timestamp, false, false);
        let value = chrono::DateTime::parse_from_rfc3339("2020-02-29T12:34:56.12345Z").unwrap();
        schema.push_table_default_value(field, DefaultValue::value(PrismaValue::DateTime(value)));
        assert!(
            default_sql(schema.walk(field))
                .unwrap_err()
                .to_string()
                .contains("100-microsecond")
        );
    }

    #[test]
    fn firebird_temporal_default_changes_are_diffed_using_canonical_sql() {
        for (native, previous, next, changed) in [
            (
                FirebirdType::Timestamp,
                "2020-02-29T12:34:56.1234Z",
                "2020-02-29T12:34:56.4321Z",
                true,
            ),
            (
                FirebirdType::Timestamp,
                "2020-02-29T12:34:56.1234+02:00",
                "2020-02-29T10:34:56.1234Z",
                false,
            ),
            (
                FirebirdType::Date,
                "2020-02-29T12:00:00Z",
                "2020-02-29T00:00:00Z",
                false,
            ),
            (
                FirebirdType::Time,
                "2020-02-29T12:34:56Z",
                "1970-01-01T12:34:56Z",
                false,
            ),
        ] {
            let mut before = empty();
            let mut after = empty();
            let previous_table = before.push_table("A".into(), NamespaceId::default(), None);
            let next_table = after.push_table("A".into(), NamespaceId::default(), None);
            let previous_id = column(&mut before, previous_table, "value", native.clone(), false, false);
            let next_id = column(&mut after, next_table, "value", native, false, false);
            before.push_table_default_value(
                previous_id,
                DefaultValue::value(PrismaValue::DateTime(
                    chrono::DateTime::parse_from_rfc3339(previous).unwrap(),
                )),
            );
            after.push_table_default_value(
                next_id,
                DefaultValue::value(PrismaValue::DateTime(
                    chrono::DateTime::parse_from_rfc3339(next).unwrap(),
                )),
            );
            let changes = all_changes(
                MigrationPair::new(before.walk(previous_id), after.walk(next_id)),
                &FirebirdDialect,
            );
            assert_eq!(changes.default_changed(), changed, "{previous} -> {next}");
        }
    }

    #[test]
    fn firebird_native_now_defaults_match_described_context_variables() {
        for (native, expression, changed) in [
            (FirebirdType::Date, "CURRENT_DATE", false),
            (FirebirdType::Time, "current_time", false),
            (FirebirdType::Timestamp, "CURRENT_TIMESTAMP", false),
            (FirebirdType::Time, "CURRENT_TIMESTAMP", true),
        ] {
            let mut before = empty();
            let mut after = empty();
            let previous_table = before.push_table("A".into(), NamespaceId::default(), None);
            let next_table = after.push_table("A".into(), NamespaceId::default(), None);
            let previous = column(&mut before, previous_table, "value", native.clone(), false, false);
            let next = column(&mut after, next_table, "value", native, false, false);
            before.push_table_default_value(previous, DefaultValue::db_generated::<String>(Some(expression.into())));
            after.push_table_default_value(next, DefaultValue::now());
            let changes = all_changes(
                MigrationPair::new(before.walk(previous), after.walk(next)),
                &FirebirdDialect,
            );
            assert_eq!(changes.default_changed(), changed, "{expression}");
        }
    }

    #[test]
    fn firebird_raw_defaults_reject_expressions_before_emitting_ddl() {
        for expression in [
            "NEXT VALUE FOR seq",
            "GEN_ID(seq, 1)",
            "'a' || 'b'",
            "GEN_UUID()",
            "(1)",
            "1; DROP TABLE A",
        ] {
            let mut after = empty();
            let table = after.push_table("A".into(), NamespaceId::default(), None);
            let field = column(&mut after, table, "value", FirebirdType::Integer, false, false);
            after.push_table_default_value(field, DefaultValue::db_generated::<String>(Some(expression.to_owned())));
            let migration = migration(empty(), after, vec![SqlMigrationStep::CreateTable { table_id: table }]);
            assert!(
                FirebirdDialect
                    .validate_migration(&migration)
                    .unwrap_err()
                    .to_string()
                    .contains("literal or context variable"),
                "{expression}"
            );
        }
    }

    #[test]
    fn firebird_rejects_altering_inherited_domain_nullability_or_default() {
        for not_null in [false, true] {
            let mut before = empty();
            let mut after = empty();
            let previous_table = before.push_table("A".into(), NamespaceId::default(), None);
            let next_table = after.push_table("A".into(), NamespaceId::default(), None);
            let previous = column(
                &mut before,
                previous_table,
                "value",
                FirebirdType::Integer,
                not_null,
                false,
            );
            let next = column(&mut after, next_table, "value", FirebirdType::Integer, false, false);
            if !not_null {
                before.push_table_default_value(previous, DefaultValue::value(1_i64));
            }
            let mut ext = FirebirdSchemaExt::default();
            ext.columns.push((
                previous,
                ColumnMetadata {
                    field_source: "user_domain".into(),
                    character_set: None,
                    collation_id: 0,
                    domain_not_null: not_null,
                    domain_default: (!not_null).then(|| "DEFAULT 1".into()),
                    column_default: None,
                },
            ));
            before.set_connector_data(Box::new(ext));
            let changes = all_changes(
                MigrationPair::new(before.walk(previous), after.walk(next)),
                &FirebirdDialect,
            );
            let migration = migration(
                before,
                after,
                vec![SqlMigrationStep::AlterTable(AlterTable {
                    table_ids: MigrationPair::new(previous_table, next_table),
                    changes: vec![TableChange::AlterColumn(AlterColumn {
                        column_id: MigrationPair::new(previous, next),
                        changes,
                        type_change: None,
                    })],
                })],
            );
            assert!(
                FirebirdDialect
                    .validate_migration(&migration)
                    .unwrap_err()
                    .to_string()
                    .contains("inherited domain")
            );
        }
    }

    #[test]
    fn firebird_create_table_quotes_names_identity_and_defaults() {
        let mut after = empty();
        let table = after.push_table("a\"b".into(), NamespaceId::default(), None);
        let id = column(&mut after, table, "id", FirebirdType::Integer, true, true);
        let text = column(&mut after, table, "title", FirebirdType::VarChar(80), true, false);
        after.push_table_default_value(text, DefaultValue::value("O'Brien"));
        index(&mut after, table, "a_pk", &[(id, None)], "pk");
        let migration = migration(empty(), after, vec![SqlMigrationStep::CreateTable { table_id: table }]);
        FirebirdDialect.validate_migration(&migration).unwrap();
        assert_eq!(
            FirebirdDialect.render_create_table(migration.after.walk(table)),
            "CREATE TABLE \"a\"\"b\" (\n    \"id\" INTEGER GENERATED BY DEFAULT AS IDENTITY NOT NULL,\n    \"title\" VARCHAR(80) CHARACTER SET UTF8 DEFAULT 'O''Brien' NOT NULL,\n    CONSTRAINT \"a_pk\" PRIMARY KEY (\"id\")\n)"
        );
    }

    #[test]
    fn firebird_unique_constraints_and_standalone_indexes_are_distinct() {
        let mut schema = empty();
        let table = schema.push_table("A".into(), NamespaceId::default(), None);
        let field = column(&mut schema, table, "value", FirebirdType::Integer, true, false);
        let unique = index(&mut schema, table, "uq", &[(field, Some(SQLSortOrder::Desc))], "unique");
        let standalone = index(&mut schema, table, "ix", &[(field, None)], "standalone");
        assert_eq!(
            FirebirdDialect.render_create_index(schema.walk(unique)),
            "ALTER TABLE \"A\" ADD CONSTRAINT \"uq\" UNIQUE (\"value\") USING DESC INDEX \"uq\""
        );
        assert_eq!(
            FirebirdDialect.render_drop_index(None, schema.walk(unique)),
            "ALTER TABLE \"A\" DROP CONSTRAINT \"uq\""
        );
        assert_eq!(
            FirebirdDialect.render_create_index(schema.walk(standalone)),
            "CREATE UNIQUE INDEX \"ix\" ON \"A\" (\"value\")"
        );
        assert_eq!(
            FirebirdDialect.render_drop_index(None, schema.walk(standalone)),
            "DROP INDEX \"ix\""
        );
        assert!(!FirebirdDialect.indexes_match(schema.walk(unique), schema.walk(standalone)));
    }

    #[test]
    fn firebird_rejects_mixed_sort_indexes_before_rendering() {
        let mut schema = empty();
        let table = schema.push_table("A".into(), NamespaceId::default(), None);
        let a = column(&mut schema, table, "a", FirebirdType::Integer, true, false);
        let b = column(&mut schema, table, "b", FirebirdType::Integer, true, false);
        index(
            &mut schema,
            table,
            "ix",
            &[(a, None), (b, Some(SQLSortOrder::Desc))],
            "normal",
        );
        assert!(
            validate_firebird_schema(&schema)
                .unwrap_err()
                .to_string()
                .contains("mixed ASC/DESC")
        );
    }

    #[test]
    fn firebird_fk_sql_uses_no_action_and_rejects_standalone_unique_target() {
        let mut after = empty();
        let parent = after.push_table("Parent".into(), NamespaceId::default(), None);
        let id = column(&mut after, parent, "id", FirebirdType::Integer, true, false);
        index(&mut after, parent, "uq", &[(id, None)], "standalone");
        let child = after.push_table("Child".into(), NamespaceId::default(), None);
        let parent_id = column(&mut after, child, "parent_id", FirebirdType::Integer, false, false);
        let fk = after.push_foreign_key(
            Some("fk".into()),
            [child, parent],
            [ForeignKeyAction::SetNull, ForeignKeyAction::Restrict],
        );
        after.push_foreign_key_column(fk, [parent_id, id]);
        assert_eq!(
            FirebirdDialect.render_add_foreign_key(after.walk(fk)),
            "ALTER TABLE \"Child\" ADD CONSTRAINT \"fk\" FOREIGN KEY (\"parent_id\") REFERENCES \"Parent\" (\"id\") ON DELETE SET NULL ON UPDATE NO ACTION"
        );
        let migration = migration(
            empty(),
            after,
            vec![SqlMigrationStep::AddForeignKey { foreign_key_id: fk }],
        );
        assert!(
            FirebirdDialect
                .validate_migration(&migration)
                .unwrap_err()
                .to_string()
                .contains("standalone UNIQUE INDEX")
        );
    }

    #[test]
    fn firebird_alter_column_widening_default_and_nullability() {
        let mut before = empty();
        let mut after = empty();
        let prev_table = before.push_table("A".into(), NamespaceId::default(), None);
        let next_table = after.push_table("A".into(), NamespaceId::default(), None);
        let prev = column(
            &mut before,
            prev_table,
            "value",
            FirebirdType::VarChar(20),
            false,
            false,
        );
        let next = column(&mut after, next_table, "value", FirebirdType::VarChar(40), true, false);
        after.push_table_default_value(next, DefaultValue::value("new"));
        let columns = MigrationPair::new(before.walk(prev), after.walk(next));
        let alter = AlterTable {
            table_ids: MigrationPair::new(prev_table, next_table),
            changes: vec![TableChange::AlterColumn(AlterColumn {
                column_id: MigrationPair::new(prev, next),
                changes: all_changes(columns, &FirebirdDialect),
                type_change: Some(ColumnTypeChange::SafeCast),
            })],
        };
        let migration = migration(before, after, vec![SqlMigrationStep::AlterTable(alter)]);
        FirebirdDialect.validate_migration(&migration).unwrap();
        let SqlMigrationStep::AlterTable(alter) = &migration.steps[0] else {
            unreachable!()
        };
        assert_eq!(
            FirebirdDialect.render_alter_table(alter, migration.schemas()),
            [
                "ALTER TABLE \"A\" ALTER \"value\" TYPE VARCHAR(40) CHARACTER SET UTF8",
                "ALTER TABLE \"A\" ALTER \"value\" SET DEFAULT 'new'",
                "ALTER TABLE \"A\" ALTER \"value\" SET NOT NULL"
            ]
        );
    }

    #[test]
    fn firebird_differ_drops_and_recreates_unique_constraint_around_widening() {
        let mut before = empty();
        let mut after = empty();
        let previous_table = before.push_table("A".into(), NamespaceId::default(), None);
        let next_table = after.push_table("A".into(), NamespaceId::default(), None);
        let previous = column(
            &mut before,
            previous_table,
            "title",
            FirebirdType::VarChar(20),
            true,
            false,
        );
        let next = column(&mut after, next_table, "title", FirebirdType::VarChar(40), true, false);
        let previous_index = index(&mut before, previous_table, "uq", &[(previous, None)], "unique");
        let next_index = index(&mut after, next_table, "uq", &[(next, None)], "unique");
        let schemas = MigrationPair::new(
            crate::SqlDatabaseSchema::from(before),
            crate::SqlDatabaseSchema::from(after),
        );
        let steps = crate::sql_schema_differ::calculate_steps(
            schemas.as_ref(),
            &FirebirdDialect,
            &schema_connector::SchemaFilter::default(),
        );
        assert!(
            matches!(&steps[..], [SqlMigrationStep::DropIndex { index_id }, SqlMigrationStep::AlterTable(_), SqlMigrationStep::CreateIndex { index_id: created, .. }] if *index_id == previous_index && *created == next_index)
        );
        let migration = migration(schemas.previous.describer_schema, schemas.next.describer_schema, steps);
        FirebirdDialect.validate_migration(&migration).unwrap();
    }

    #[test]
    fn firebird_rejects_identity_conversion_and_column_replacement() {
        for identity in [false, true] {
            let mut before = empty();
            let mut after = empty();
            let prev_table = before.push_table("A".into(), NamespaceId::default(), None);
            let next_table = after.push_table("A".into(), NamespaceId::default(), None);
            let prev = column(&mut before, prev_table, "value", FirebirdType::Integer, true, false);
            let next = column(
                &mut after,
                next_table,
                "value",
                if identity {
                    FirebirdType::Integer
                } else {
                    FirebirdType::SmallInt
                },
                true,
                identity,
            );
            let changes = all_changes(
                MigrationPair::new(before.walk(prev), after.walk(next)),
                &FirebirdDialect,
            );
            let change = if identity {
                TableChange::AlterColumn(AlterColumn {
                    column_id: MigrationPair::new(prev, next),
                    changes,
                    type_change: None,
                })
            } else {
                TableChange::DropAndRecreateColumn {
                    column_id: MigrationPair::new(prev, next),
                    changes,
                }
            };
            let migration = migration(
                before,
                after,
                vec![SqlMigrationStep::AlterTable(AlterTable {
                    table_ids: MigrationPair::new(prev_table, next_table),
                    changes: vec![change],
                })],
            );
            assert!(FirebirdDialect.validate_migration(&migration).is_err());
        }
    }

    #[test]
    fn firebird_rejects_dropping_key_with_retained_foreign_key() {
        let mut before = empty();
        let parent = before.push_table("Parent".into(), NamespaceId::default(), None);
        let id = column(&mut before, parent, "id", FirebirdType::Integer, true, false);
        let uq = index(&mut before, parent, "uq", &[(id, None)], "unique");
        let child = before.push_table("Child".into(), NamespaceId::default(), None);
        let parent_id = column(&mut before, child, "parent_id", FirebirdType::Integer, true, false);
        let fk = before.push_foreign_key(
            Some("fk".into()),
            [child, parent],
            [ForeignKeyAction::NoAction, ForeignKeyAction::NoAction],
        );
        before.push_foreign_key_column(fk, [parent_id, id]);
        let migration = migration(before, empty(), vec![SqlMigrationStep::DropIndex { index_id: uq }]);
        assert!(
            FirebirdDialect
                .validate_migration(&migration)
                .unwrap_err()
                .to_string()
                .contains("explicitly dropping and recreating foreign key")
        );
    }

    #[test]
    fn firebird_unknown_column_creation_and_empty_dbgenerated_are_explicit_errors() {
        let mut after = empty();
        let table = after.push_table("A".into(), NamespaceId::default(), None);
        let id = column(&mut after, table, "id", FirebirdType::Integer, true, false);
        after.push_table_default_value(id, DefaultValue::db_generated::<String>(None));
        let migration = migration(empty(), after, vec![SqlMigrationStep::CreateTable { table_id: table }]);
        assert!(
            FirebirdDialect
                .validate_migration(&migration)
                .unwrap_err()
                .to_string()
                .contains("literal or context variable")
        );
    }
}

fn column_definition(column: TableColumnWalker<'_>) -> String {
    let mut result = format!(
        "{} {}",
        quote_identifier(column.name()),
        column_type_sql(column).expect("validated Firebird column")
    );
    if column.is_autoincrement() {
        result.push_str(" GENERATED BY DEFAULT AS IDENTITY");
    }
    if let Some(default) = default_sql(column).expect("validated Firebird default") {
        result.push_str(" DEFAULT ");
        result.push_str(&default);
    }
    if column.arity().is_required() {
        result.push_str(" NOT NULL");
    }
    result
}

fn column_names(index: IndexWalker<'_>) -> String {
    index
        .column_names()
        .map(quote_identifier)
        .collect::<Vec<_>>()
        .join(", ")
}
fn descending(index: IndexWalker<'_>) -> bool {
    index
        .columns()
        .next()
        .is_some_and(|c| c.sort_order() == Some(SQLSortOrder::Desc))
}
fn constraint_name(index: IndexWalker<'_>) -> Option<&str> {
    let ext: &FirebirdSchemaExt = index.schema.downcast_connector_data();
    ext.index_metadata(index.id)
        .and_then(|meta| meta.constraint_name.as_deref())
}
fn physical_index_name(index: IndexWalker<'_>) -> &str {
    let ext: &FirebirdSchemaExt = index.schema.downcast_connector_data();
    ext.index_metadata(index.id)
        .map(|meta| meta.database_name.as_str())
        .unwrap_or_else(|| index.name())
}
fn key_definition(index: IndexWalker<'_>) -> String {
    let mut sql = format!(
        "CONSTRAINT {} {} ({})",
        quote_identifier(index.name()),
        if index.is_primary_key() {
            "PRIMARY KEY"
        } else {
            "UNIQUE"
        },
        column_names(index)
    );
    if descending(index) {
        sql.push_str(&format!(" USING DESC INDEX {}", quote_identifier(index.name())));
    }
    sql
}
fn create_table(table: TableWalker<'_>, name: &str) -> String {
    let mut definitions = table.columns().map(column_definition).collect::<Vec<_>>();
    if let Some(pk) = table.primary_key() {
        definitions.push(key_definition(pk));
    }
    format!(
        "CREATE TABLE {} (\n    {}\n)",
        quote_identifier(name),
        definitions.join(",\n    ")
    )
}
fn action(action: ForeignKeyAction) -> &'static str {
    match action {
        ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => "NO ACTION",
        ForeignKeyAction::Cascade => "CASCADE",
        ForeignKeyAction::SetNull => "SET NULL",
        ForeignKeyAction::SetDefault => "SET DEFAULT",
    }
}

fn check_referenced_key_drop(index: IndexWalker<'_>, migration: &SqlMigration) -> ConnectorResult<()> {
    for fk in migration.before.walk_foreign_keys().filter(|fk| {
        fk.referenced_table().id == index.table().id
            && fk.referenced_columns().map(|c| c.name()).eq(index.column_names())
    }) {
        let dropped = migration.steps.iter().any(
            |step| matches!(step, SqlMigrationStep::DropForeignKey { foreign_key_id } if *foreign_key_id == fk.id),
        );
        if !dropped {
            return Err(unsupported(format!(
                "dropping or renaming referenced constraint {:?} requires explicitly dropping and recreating foreign key {:?}",
                index.name(),
                fk.constraint_name()
            )));
        }
    }
    Ok(())
}

impl SqlRenderer for FirebirdDialect {
    fn validate_migration(&self, migration: &SqlMigration) -> ConnectorResult<()> {
        validate_firebird_schema(&migration.before)?;
        validate_firebird_schema(&migration.after)?;
        for step in &migration.steps {
            match step {
                SqlMigrationStep::CreateTable { table_id } => {
                    for column in migration.after.walk(*table_id).columns() {
                        validate_column(column)?;
                    }
                }
                SqlMigrationStep::AlterTable(alter) => {
                    let tables = migration.schemas().walk(alter.table_ids);
                    for change in &alter.changes {
                        match change {
                            TableChange::AddColumn { column_id, .. } => {
                                validate_column(migration.after.walk(*column_id))?
                            }
                            TableChange::AlterColumn(alter) => {
                                let columns = migration.schemas().walk(alter.column_id);
                                let ext: &FirebirdSchemaExt = migration.before.downcast_connector_data();
                                if ext.columns.iter().any(|(id, meta)| {
                                    *id == columns.previous.id
                                        && (meta.domain_not_null || meta.domain_default.is_some())
                                }) {
                                    return Err(unsupported(
                                        "altering a column with inherited domain defaults or NOT NULL requires a reviewed SQL migration",
                                    ));
                                }
                                validate_column(columns.next)?;
                                if alter.changes.autoincrement_changed()
                                    || (alter.changes.type_changed() && columns.previous.is_autoincrement())
                                {
                                    return Err(unsupported(
                                        "altering identity generation or an identity column's type",
                                    ));
                                }
                                if alter.changes.type_changed()
                                    && !matches!(
                                        alter.type_change,
                                        Some(crate::sql_migration::ColumnTypeChange::SafeCast)
                                    )
                                {
                                    return Err(unsupported(
                                        "only explicitly supported type widening can be applied automatically",
                                    ));
                                }
                                if alter.changes.type_changed() {
                                    for index in tables
                                        .previous
                                        .indexes()
                                        .filter(|index| index.contains_column(columns.previous.id))
                                    {
                                        let dropped = if index.is_primary_key() {
                                            migration.steps.iter().any(|step| matches!(step, SqlMigrationStep::AlterTable(table) if table.table_ids.previous == tables.previous.id && table.changes.iter().any(|change| matches!(change, TableChange::DropPrimaryKey))))
                                        } else {
                                            migration.steps.iter().any(|step| matches!(step, SqlMigrationStep::DropIndex { index_id } if *index_id == index.id))
                                        };
                                        if !dropped {
                                            return Err(unsupported(format!(
                                                "type widening requires dropping and recreating index {:?} before altering the column",
                                                index.name()
                                            )));
                                        }
                                    }
                                    for fk in migration.before.walk_foreign_keys().filter(|fk| {
                                        fk.constrained_columns()
                                            .chain(fk.referenced_columns())
                                            .any(|column| column.id == columns.previous.id)
                                    }) {
                                        if !migration.steps.iter().any(|step| matches!(step, SqlMigrationStep::DropForeignKey { foreign_key_id } if *foreign_key_id == fk.id)) {
                                            return Err(unsupported("type widening of a foreign-key column requires dropping and recreating the foreign key"));
                                        }
                                    }
                                }
                            }
                            TableChange::DropAndRecreateColumn { .. } => {
                                return Err(unsupported(
                                    "column replacement and non-widening type changes require a reviewed SQL migration",
                                ));
                            }
                            TableChange::DropPrimaryKey | TableChange::RenamePrimaryKey => {
                                if let Some(pk) = tables.previous.primary_key() {
                                    check_referenced_key_drop(pk, migration)?;
                                }
                            }
                            TableChange::AddPrimaryKey | TableChange::DropColumn { .. } => (),
                        }
                    }
                }
                SqlMigrationStep::DropIndex { index_id } => {
                    let index = migration.before.walk(*index_id);
                    if index.is_unique() && constraint_name(index).is_some() {
                        check_referenced_key_drop(index, migration)?;
                    }
                }
                SqlMigrationStep::RedefineIndex { index } => {
                    let index = migration.before.walk(index.previous);
                    if index.is_unique() && constraint_name(index).is_some() {
                        check_referenced_key_drop(index, migration)?;
                    }
                }
                SqlMigrationStep::AddForeignKey { foreign_key_id } => {
                    let fk = migration.after.walk(*foreign_key_id);
                    let referenced = fk.referenced_table().indexes().any(|index| {
                        (index.is_primary_key() || (index.is_unique() && constraint_name(index).is_some()))
                            && index.column_names().eq(fk.referenced_columns().map(|c| c.name()))
                    });
                    if !referenced {
                        return Err(unsupported(
                            "foreign keys must reference a PRIMARY KEY or UNIQUE constraint, not a standalone UNIQUE INDEX",
                        ));
                    }
                }
                SqlMigrationStep::DropTable { .. }
                | SqlMigrationStep::DropForeignKey { .. }
                | SqlMigrationStep::CreateIndex { .. } => (),
                _ => {
                    return Err(unsupported(format!(
                        "migration operation {} is not implemented",
                        step.description()
                    )));
                }
            }
        }
        Ok(())
    }

    fn quote<'a>(&self, name: &'a str) -> Quoted<&'a str> {
        Quoted::Double(name)
    }

    fn render_create_table(&self, table: TableWalker<'_>) -> String {
        create_table(table, table.name())
    }
    fn render_create_table_as(&self, table: TableWalker<'_>, table_name: QuotedWithPrefix<&str>) -> String {
        assert!(
            table_name.0.is_none(),
            "Firebird namespaces are rejected before rendering"
        );
        let name = match table_name.1 {
            Quoted::Double(name) | Quoted::Single(name) | Quoted::Backticks(name) | Quoted::SquareBrackets(name) => {
                name
            }
        };
        create_table(table, name)
    }
    fn render_drop_table(&self, _namespace: Option<&str>, name: &str) -> Vec<String> {
        vec![format!("DROP TABLE {}", quote_identifier(name))]
    }

    fn render_create_index(&self, index: IndexWalker<'_>) -> String {
        if index.is_unique() && constraint_name(index).is_some() {
            return format!(
                "ALTER TABLE {} ADD {}",
                quote_identifier(index.table().name()),
                key_definition(index)
            );
        }
        format!(
            "CREATE {}{}INDEX {} ON {} ({})",
            if index.is_unique() { "UNIQUE " } else { "" },
            if descending(index) { "DESCENDING " } else { "" },
            quote_identifier(physical_index_name(index)),
            quote_identifier(index.table().name()),
            column_names(index)
        )
    }
    fn render_drop_index(&self, _namespace: Option<&str>, index: IndexWalker<'_>) -> String {
        match constraint_name(index) {
            Some(name) => format!(
                "ALTER TABLE {} DROP CONSTRAINT {}",
                quote_identifier(index.table().name()),
                quote_identifier(name)
            ),
            None => format!("DROP INDEX {}", quote_identifier(physical_index_name(index))),
        }
    }
    fn render_drop_and_recreate_index(&self, indexes: MigrationPair<IndexWalker<'_>>) -> Vec<String> {
        vec![
            self.render_drop_index(None, indexes.previous),
            self.render_create_index(indexes.next),
        ]
    }

    fn render_add_foreign_key(&self, fk: ForeignKeyWalker<'_>) -> String {
        format!(
            "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({}) ON DELETE {} ON UPDATE {}",
            quote_identifier(fk.table().name()),
            quote_identifier(fk.constraint_name().expect("validated named foreign key")),
            fk.constrained_columns()
                .map(|c| quote_identifier(c.name()))
                .collect::<Vec<_>>()
                .join(", "),
            quote_identifier(fk.referenced_table().name()),
            fk.referenced_columns()
                .map(|c| quote_identifier(c.name()))
                .collect::<Vec<_>>()
                .join(", "),
            action(fk.on_delete_action()),
            action(fk.on_update_action())
        )
    }
    fn render_drop_foreign_key(&self, _namespace: Option<&str>, fk: ForeignKeyWalker<'_>) -> String {
        format!(
            "ALTER TABLE {} DROP CONSTRAINT {}",
            quote_identifier(fk.table().name()),
            quote_identifier(fk.constraint_name().expect("validated named foreign key"))
        )
    }

    fn render_alter_table(&self, alter: &AlterTable, schemas: MigrationPair<&SqlSchema>) -> Vec<String> {
        let tables = schemas.walk(alter.table_ids);
        let prefix = format!("ALTER TABLE {}", quote_identifier(tables.next.name()));
        let mut statements = Vec::new();
        for change in &alter.changes {
            match change {
                TableChange::AddColumn { column_id, .. } => statements.push(format!(
                    "{prefix} ADD {}",
                    column_definition(schemas.next.walk(*column_id))
                )),
                TableChange::DropColumn { column_id } => statements.push(format!(
                    "{prefix} DROP {}",
                    quote_identifier(schemas.previous.walk(*column_id).name())
                )),
                TableChange::AlterColumn(alter) => {
                    let columns = schemas.walk(alter.column_id);
                    let column = quote_identifier(columns.next.name());
                    if alter.changes.type_changed() {
                        statements.push(format!(
                            "{prefix} ALTER {column} TYPE {}",
                            column_type_sql(columns.next).expect("validated Firebird type")
                        ));
                    }
                    if alter.changes.default_changed() {
                        statements.push(match default_sql(columns.next).expect("validated Firebird default") {
                            Some(default) => format!("{prefix} ALTER {column} SET DEFAULT {default}"),
                            None => format!("{prefix} ALTER {column} DROP DEFAULT"),
                        });
                    }
                    if alter.changes.arity_changed() {
                        statements.push(format!(
                            "{prefix} ALTER {column} {} NOT NULL",
                            if columns.next.arity().is_required() {
                                "SET"
                            } else {
                                "DROP"
                            }
                        ));
                    }
                }
                TableChange::DropPrimaryKey => statements.push(format!(
                    "{prefix} DROP CONSTRAINT {}",
                    quote_identifier(tables.previous.primary_key().expect("previous primary key").name())
                )),
                TableChange::AddPrimaryKey => statements.push(format!(
                    "{prefix} ADD {}",
                    key_definition(tables.next.primary_key().expect("next primary key"))
                )),
                TableChange::RenamePrimaryKey => {
                    statements.push(format!(
                        "{prefix} DROP CONSTRAINT {}",
                        quote_identifier(tables.previous.primary_key().expect("previous primary key").name())
                    ));
                    statements.push(format!(
                        "{prefix} ADD {}",
                        key_definition(tables.next.primary_key().expect("next primary key"))
                    ));
                }
                TableChange::DropAndRecreateColumn { .. } => unreachable!("preflight rejects column replacement"),
            }
        }
        statements
    }

    fn render_alter_enum(&self, _alter: &AlterEnum, _schemas: MigrationPair<&SqlSchema>) -> Vec<String> {
        unreachable!("preflight rejects enums")
    }
    fn render_create_enum(&self, _enm: EnumWalker<'_>) -> Vec<String> {
        unreachable!("preflight rejects enums")
    }
    fn render_drop_enum(&self, _namespace: Option<&str>, _enm: EnumWalker<'_>) -> Vec<String> {
        unreachable!("preflight rejects enums")
    }
    fn render_redefine_tables(&self, _tables: &[RedefineTable], _schemas: MigrationPair<&SqlSchema>) -> Vec<String> {
        unreachable!("preflight rejects table replacement")
    }
    fn render_rename_table(&self, _namespace: Option<&str>, _name: &str, _new_name: &str) -> String {
        unreachable!("Firebird 3 has no RENAME TABLE")
    }
    fn render_drop_view(&self, _namespace: Option<&str>, _view: ViewWalker<'_>) -> String {
        unreachable!("preflight rejects views")
    }
    fn render_drop_user_defined_type(&self, _namespace: Option<&str>, _udt: &UserDefinedTypeWalker<'_>) -> String {
        unreachable!("preflight rejects user-defined types")
    }
    fn render_rename_foreign_key(&self, _fks: MigrationPair<ForeignKeyWalker<'_>>) -> String {
        unreachable!("Firebird foreign keys are dropped and recreated")
    }
}
