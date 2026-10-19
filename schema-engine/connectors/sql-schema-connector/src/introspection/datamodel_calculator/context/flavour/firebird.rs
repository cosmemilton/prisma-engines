// Added for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.

pub(crate) struct FirebirdIntrospectionFlavour;

impl super::IntrospectionFlavour for FirebirdIntrospectionFlavour {}

#[cfg(test)]
mod tests {
    use psl::{
        builtin_connectors::FirebirdType, datamodel_connector::NativeTypeInstance, parser_database::NoExtensionTypes,
    };
    use schema_connector::{CompositeTypeDepth, IntrospectionContext};
    use sql_schema_describer::{
        Column, ColumnArity, ColumnType, ColumnTypeFamily, DefaultValue, IndexColumn, SqlSchema,
        firebird::FirebirdSchemaExt,
    };

    #[test]
    fn firebird_introspection_renders_identity_native_precision_and_existing_mappings() {
        let previous = psl::validate_without_extensions(
            r#"
datasource db {
  provider = "firebird"
}

model Product {
  id Int @id @map("ID")
  amount Decimal? @db.Decimal(18, 4) @map("AMOUNT")
  label String? @db.VarChar(42) @map("LABEL")
  optionalNull Int? @map("OPTIONAL_NULL")
  @@map("FB_PRODUCT")
}
"#
            .into(),
        );
        assert!(!previous.diagnostics.has_errors(), "{:?}", previous.diagnostics);
        let ctx = IntrospectionContext::new(previous, CompositeTypeDepth::None, None, Default::default())
            .without_config_rendering();
        let mut schema = SqlSchema::default();
        let namespace = sql_schema_describer::NamespaceId::default();
        let table = schema.push_table("FB_PRODUCT".into(), namespace, None);
        let id = schema.push_table_column(
            table,
            Column {
                name: "ID".into(),
                tpe: ColumnType {
                    full_data_type: "INTEGER".into(),
                    family: ColumnTypeFamily::Int,
                    arity: ColumnArity::Required,
                    native_type: Some(NativeTypeInstance::new(FirebirdType::Integer)),
                },
                auto_increment: true,
                description: None,
            },
        );
        schema.push_table_column(
            table,
            Column {
                name: "AMOUNT".into(),
                tpe: ColumnType {
                    full_data_type: "DECIMAL(18,4)".into(),
                    family: ColumnTypeFamily::Decimal,
                    arity: ColumnArity::Nullable,
                    native_type: Some(NativeTypeInstance::new(FirebirdType::Decimal(Some((18, 4))))),
                },
                auto_increment: false,
                description: None,
            },
        );
        let label = schema.push_table_column(
            table,
            Column {
                name: "LABEL".into(),
                tpe: ColumnType {
                    full_data_type: "VARCHAR(42)".into(),
                    family: ColumnTypeFamily::String,
                    arity: ColumnArity::Nullable,
                    native_type: Some(NativeTypeInstance::new(FirebirdType::VarChar(42))),
                },
                auto_increment: false,
                description: None,
            },
        );
        schema.push_table_default_value(label, DefaultValue::value("it's preserved"));
        let optional_null = schema.push_table_column(
            table,
            Column {
                name: "OPTIONAL_NULL".into(),
                tpe: ColumnType {
                    full_data_type: "INTEGER".into(),
                    family: ColumnTypeFamily::Int,
                    arity: ColumnArity::Nullable,
                    native_type: Some(NativeTypeInstance::new(FirebirdType::Integer)),
                },
                auto_increment: false,
                description: None,
            },
        );
        schema.push_table_default_value(optional_null, DefaultValue::db_generated("NULL"));
        let pk = schema.push_primary_key(table, "PK_FB_PRODUCT".into());
        schema.push_index_column(IndexColumn {
            index_id: pk,
            column_id: id,
            sort_order: None,
            length: None,
        });
        schema.set_connector_data(Box::new(FirebirdSchemaExt::default()));
        let result = crate::introspection::datamodel_calculator::calculate(&schema, &ctx, "", &NoExtensionTypes);
        let output = result.single_datamodel();
        for expected in [
            "model Product",
            "@default(autoincrement())",
            "@db.Decimal(18, 4)",
            "@db.VarChar(42)",
            "@map(\"ID\")",
            "@@map(\"FB_PRODUCT\")",
            "@default(\"it's preserved\")",
            "@default(dbgenerated(\"NULL\"))",
        ] {
            assert!(output.contains(expected), "Missing {expected:?} in {output}");
        }
        assert!(!output.contains("@@schema"), "{output}");
        let validated = psl::validate_without_extensions(
            format!("datasource db {{\n provider = \"firebird\"\n}}\n{output}").into(),
        );
        assert!(!validated.diagnostics.has_errors(), "{:?}", validated.diagnostics);
    }
}
