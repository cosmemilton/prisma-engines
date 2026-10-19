#![cfg(feature = "firebird")]
// Modified for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.

use psl::{
    Diagnostics, ValidatedSchema,
    builtin_connectors::{FIREBIRD, FirebirdType},
    datamodel_connector::{ConnectorCapability, Flavour, RelationMode},
    diagnostics::Span,
    parser_database::{NoExtensionTypes, ScalarFieldType, ScalarType},
};

const SOURCE: &str = r#"
datasource db {
  provider = "firebird"
}
"#;

fn validate(models: &str) -> ValidatedSchema {
    psl::validate_without_extensions(format!("{SOURCE}\n{models}").into())
}

#[track_caller]
fn assert_valid(models: &str) -> ValidatedSchema {
    let schema = validate(models);
    assert!(!schema.diagnostics.has_errors(), "{}", schema.render_own_diagnostics());
    schema
}

#[track_caller]
fn assert_single_error(schema: &ValidatedSchema, expected_parts: &[&str]) {
    let errors = schema.diagnostics.errors();
    assert_eq!(errors.len(), 1, "{}", schema.render_own_diagnostics());
    for expected in expected_parts {
        assert!(
            errors[0].message().contains(expected),
            "Expected {expected:?} in diagnostic {:?}",
            errors[0].message()
        );
    }
}

#[test]
fn provider_resolves_to_the_firebird_connector() {
    let schema = assert_valid("model Item {\n  id Int @id\n}");

    assert_eq!(schema.connector.provider_name(), "firebird");
    assert_eq!(schema.connector.flavour(), Flavour::Firebird);
    assert!(schema.connector.is_sql());
    assert_eq!(schema.configuration.datasources[0].active_provider, "firebird");
    assert_eq!(schema.relation_mode(), RelationMode::ForeignKeys);

    let capabilities = schema.connector.capabilities();
    for capability in [
        ConnectorCapability::AnyId,
        ConnectorCapability::AutoIncrement,
        ConnectorCapability::CompoundIds,
        ConnectorCapability::CreateMany,
        ConnectorCapability::CreateManyWriteableAutoIncId,
        ConnectorCapability::DecimalType,
        ConnectorCapability::InsertReturning,
        ConnectorCapability::NamedPrimaryKeys,
        ConnectorCapability::NamedForeignKeys,
        ConnectorCapability::UpdateableId,
        ConnectorCapability::SupportsTxIsolationReadCommitted,
        ConnectorCapability::SupportsTxIsolationRepeatableRead,
        ConnectorCapability::SupportsTxIsolationSnapshot,
        ConnectorCapability::SupportsTxIsolationSerializable,
    ] {
        assert!(capabilities.contains(capability));
    }
    assert_eq!(capabilities.iter().count(), 14);
    assert!(!capabilities.contains(ConnectorCapability::UpdateReturning));
    assert!(!capabilities.contains(ConnectorCapability::SupportsDefaultInInsert));
    assert!(!capabilities.contains(ConnectorCapability::CreateSkipDuplicates));
}

#[test]
fn native_types_round_trip_and_map_to_prisma_scalars() {
    let cases: &[(&str, &[&str], FirebirdType, ScalarType)] = &[
        ("SmallInt", &[], FirebirdType::SmallInt, ScalarType::Int),
        ("Integer", &[], FirebirdType::Integer, ScalarType::Int),
        ("BigInt", &[], FirebirdType::BigInt, ScalarType::BigInt),
        ("Float", &[], FirebirdType::Float, ScalarType::Float),
        ("DoublePrecision", &[], FirebirdType::DoublePrecision, ScalarType::Float),
        ("Decimal", &[], FirebirdType::Decimal(None), ScalarType::Decimal),
        (
            "Decimal",
            &["18", "6"],
            FirebirdType::Decimal(Some((18, 6))),
            ScalarType::Decimal,
        ),
        ("Numeric", &[], FirebirdType::Numeric(None), ScalarType::Decimal),
        (
            "Numeric",
            &["10", "2"],
            FirebirdType::Numeric(Some((10, 2))),
            ScalarType::Decimal,
        ),
        ("Boolean", &[], FirebirdType::Boolean, ScalarType::Boolean),
        ("Char", &["1"], FirebirdType::Char(1), ScalarType::String),
        ("VarChar", &["8191"], FirebirdType::VarChar(8191), ScalarType::String),
        ("Text", &[], FirebirdType::Text, ScalarType::String),
        ("Blob", &[], FirebirdType::Blob, ScalarType::Bytes),
        ("Date", &[], FirebirdType::Date, ScalarType::DateTime),
        ("Time", &[], FirebirdType::Time, ScalarType::DateTime),
        ("Timestamp", &[], FirebirdType::Timestamp, ScalarType::DateTime),
    ];

    for (name, args, expected, scalar) in cases {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let mut diagnostics = Diagnostics::new();
        let native = FIREBIRD
            .parse_native_type(name, &args, Span::empty(), &mut diagnostics)
            .unwrap_or_else(|| panic!("Could not parse {name}: {diagnostics:?}"));

        assert!(!diagnostics.has_errors(), "{name}: {diagnostics:?}");
        assert_eq!(native.downcast_ref::<FirebirdType>(), expected);
        assert_eq!(
            FIREBIRD.scalar_type_for_native_type(&native, &NoExtensionTypes),
            Some(ScalarFieldType::BuiltInScalar(*scalar)),
            "{name}"
        );

        let (round_trip_name, round_trip_args) = FIREBIRD.native_type_to_parts(&native);
        assert_eq!(round_trip_name, *name);
        assert_eq!(round_trip_args.as_ref(), args.as_slice());
    }
}

#[test]
fn default_native_types_match_the_firebird_3_baseline() {
    let schema = assert_valid("model Item {\n  id Int @id\n}");
    let cases = [
        (ScalarType::Int, FirebirdType::Integer),
        (ScalarType::BigInt, FirebirdType::BigInt),
        (ScalarType::Float, FirebirdType::DoublePrecision),
        (ScalarType::Decimal, FirebirdType::Decimal(Some((18, 6)))),
        (ScalarType::Boolean, FirebirdType::Boolean),
        (ScalarType::String, FirebirdType::VarChar(191)),
        (ScalarType::DateTime, FirebirdType::Timestamp),
        (ScalarType::Bytes, FirebirdType::Blob),
    ];

    for (scalar, expected) in cases {
        let native = FIREBIRD
            .default_native_type_for_scalar_type(&ScalarFieldType::BuiltInScalar(scalar), &schema)
            .unwrap_or_else(|| panic!("Missing default native type for {scalar:?}"));
        assert_eq!(native.downcast_ref::<FirebirdType>(), &expected);
    }

    assert!(
        FIREBIRD
            .default_native_type_for_scalar_type(&ScalarFieldType::BuiltInScalar(ScalarType::Json), &schema)
            .is_none()
    );
}

#[test]
fn supported_native_types_and_boundary_arguments_validate() {
    assert_valid(
        r#"
model Item {
  id Int @id @db.Integer
  small Int @db.SmallInt
  big BigInt @db.BigInt
  single Float @db.Float
  double Float @db.DoublePrecision
  decimal Decimal @db.Decimal
  numeric Decimal @db.Numeric
  decimalMin Decimal @db.Decimal(1, 0)
  decimalMax Decimal @db.Decimal(18, 18)
  numericMin Decimal @db.Numeric(1, 1)
  numericMax Decimal @db.Numeric(18, 0)
  enabled Boolean @db.Boolean
  charMin String @db.Char(1)
  charMax String @db.Char(8191)
  varcharMin String @db.VarChar(1)
  varcharMax String @db.VarChar(8191)
  text String @db.Text
  binary Bytes @db.Blob
  date DateTime @db.Date
  time DateTime @db.Time
  timestamp DateTime @db.Timestamp
}
"#,
    );
}

#[test]
fn string_ids_and_named_compound_keys_and_foreign_keys_validate() {
    assert_valid(
        r#"
model Parent {
  tenant Int
  id Int
  children Child[]

  @@id([tenant, id], map: "parent_pk")
}

model Child {
  id String @id(map: "child_pk")
  tenant Int
  parentId Int
  parent Parent @relation(fields: [tenant, parentId], references: [tenant, id], map: "child_parent_fk")
}
"#,
    );
}

#[test]
fn database_table_names_longer_than_31_bytes_are_rejected() {
    let name = "T".repeat(32);
    let schema = validate(&format!("model {name} {{\n  id Int @id\n}}"));
    assert_single_error(&schema, &["database table name", &name, "31 bytes"]);

    for name in ["T".repeat(32), "é".repeat(16)] {
        let schema = validate(&format!("model Item {{\n  id Int @id\n  @@map(\"{name}\")\n}}"));
        assert_single_error(&schema, &["database table name", &name, "31 bytes"]);
    }
}

#[test]
fn database_column_names_longer_than_31_bytes_are_rejected() {
    let name = "c".repeat(32);
    let schema = validate(&format!("model Item {{\n  id Int @id\n  {name} String\n}}"));
    assert_single_error(&schema, &["database column name", &name, "31 bytes"]);

    for name in ["c".repeat(32), "é".repeat(16)] {
        let schema = validate(&format!(
            "model Item {{\n  id Int @id\n  value String @map(\"{name}\")\n}}"
        ));
        assert_single_error(&schema, &["database column name", &name, "31 bytes"]);
    }
}

#[test]
fn database_names_at_the_31_byte_boundary_validate() {
    let table = "T".repeat(31);
    let column = "c".repeat(31);
    assert_valid(&format!("model {table} {{\n  id Int @id\n  {column} String\n}}"));

    for name in ["m".repeat(31), format!("{}x", "é".repeat(15))] {
        assert_eq!(name.len(), 31);
        assert_valid(&format!(
            "model Item {{\n  id Int @id\n  value String @map(\"{name}\")\n  @@map(\"{name}\")\n}}"
        ));
    }
}

#[test]
fn long_prisma_names_can_map_to_short_database_names() {
    let model = "M".repeat(32);
    let field = "f".repeat(32);
    assert_valid(&format!(
        "model {model} {{\n  id Int @id\n  {field} String @map(\"value\")\n  @@map(\"items\")\n}}"
    ));
}

#[test]
fn text_and_blob_native_types_cannot_be_keys_or_indexes() {
    for (scalar, native) in [("String", "Text"), ("Bytes", "Blob")] {
        let schema = validate(&format!("model Item {{\n  value {scalar} @id @db.{native}\n}}"));
        assert_single_error(&schema, &[native, "cannot be used on a field that is `@id` or `@@id`"]);

        let schema = validate(&format!(
            "model Item {{\n  id Int @id\n  value {scalar} @unique @db.{native}\n}}"
        ));
        assert_single_error(&schema, &[native, "cannot be unique"]);

        let schema = validate(&format!(
            "model Item {{\n  id Int @id\n  value {scalar} @db.{native}\n  @@index([value])\n}}"
        ));
        assert_single_error(&schema, &["cannot define an index", native]);
    }
}

#[test]
fn default_bytes_blob_cannot_be_a_key_or_index() {
    let schema = validate("model Item {\n  value Bytes @id\n}");
    assert_single_error(&schema, &["Blob", "cannot be used on a field that is `@id` or `@@id`"]);

    let schema = validate("model Item {\n  id Int @id\n  value Bytes @unique\n}");
    assert_single_error(&schema, &["Blob", "cannot be unique"]);

    let schema = validate("model Item {\n  id Int @id\n  value Bytes\n  @@index([value])\n}");
    assert_single_error(&schema, &["cannot define an index", "Blob"]);
}

#[test]
fn decimal_precision_must_be_between_one_and_eighteen() {
    for native in ["Decimal", "Numeric"] {
        for precision in [0, 19] {
            let schema = validate(&format!(
                "model Item {{\n  id Int @id\n  value Decimal @db.{native}({precision}, 0)\n}}"
            ));
            assert_single_error(&schema, &[native, "Precision can range from 1 to 18"]);
        }
    }
}

#[test]
fn decimal_scale_must_not_exceed_precision() {
    for native in ["Decimal", "Numeric"] {
        for (precision, scale) in [(1, 2), (18, 19)] {
            let schema = validate(&format!(
                "model Item {{\n  id Int @id\n  value Decimal @db.{native}({precision}, {scale})\n}}"
            ));
            assert_single_error(&schema, &[native, "scale must not be larger than the precision"]);
        }
    }
}

#[test]
fn character_lengths_must_be_between_one_and_8191_for_utf8() {
    for native in ["Char", "VarChar"] {
        for length in [0, 8192] {
            let schema = validate(&format!(
                "model Item {{\n  id Int @id\n  value String @db.{native}({length})\n}}"
            ));
            assert_single_error(&schema, &[native, "Length can range from 1 to 8191", "UTF8"]);
        }
    }
}

#[test]
fn unknown_native_types_are_rejected() {
    let schema = validate("model Item {\n  id Int @id\n  value String @db.Unknown\n}");
    assert_single_error(
        &schema,
        &["Native type Unknown is not supported for firebird connector"],
    );
}

#[test]
fn native_types_must_match_the_prisma_scalar() {
    let schema = validate("model Item {\n  id Int @id\n  value String @db.Integer\n}");
    assert_single_error(
        &schema,
        &[
            "Native type Integer is not compatible with declared field type String",
            "expected field type Int",
        ],
    );
}

#[test]
fn json_is_rejected() {
    let schema = validate("model Item {\n  id Int @id\n  value Json\n}");
    assert_single_error(&schema, &["current connector does not support the Json type"]);
}

#[test]
fn enums_are_rejected() {
    let schema = validate(
        r#"
enum State {
  OPEN
  CLOSED
}

model Item {
  id Int @id
  state State
}
"#,
    );
    assert_single_error(&schema, &["enum `State`", "current connector does not support enums"]);
}

#[test]
fn scalar_lists_are_rejected() {
    let schema = validate("model Item {\n  id Int @id\n  values String[]\n}");
    assert_single_error(&schema, &["does not support lists of primitive types"]);
}

#[test]
fn autoincrement_accepts_integer_identity_primary_keys() {
    for field_type in ["Int", "Int @db.SmallInt", "BigInt"] {
        assert_valid(&format!(
            "model Item {{\n  id {field_type} @id @default(autoincrement()) @map(\"ID\")\n}}"
        ));
    }
}

#[test]
fn autoincrement_remains_limited_to_a_single_primary_key() {
    let schema = validate("model Item {\n  id Int @id\n  value Int @unique @default(autoincrement())\n}");
    assert_single_error(&schema, &["`autoincrement()` default value is used on a non-id field"]);
}

#[test]
fn foreign_keys_is_the_only_allowed_relation_mode() {
    assert_eq!(
        FIREBIRD.allowed_relation_mode_settings().iter().collect::<Vec<_>>(),
        vec![RelationMode::ForeignKeys]
    );

    let schema = psl::validate_without_extensions(
        r#"
datasource db {
  provider = "firebird"
  relationMode = "foreignKeys"
}

model Item {
  id Int @id
}
"#
        .into(),
    );
    assert!(!schema.diagnostics.has_errors(), "{}", schema.render_own_diagnostics());
    assert_eq!(schema.relation_mode(), RelationMode::ForeignKeys);

    let schema = psl::validate_without_extensions(
        r#"
datasource db {
  provider = "firebird"
  relationMode = "prisma"
}

model Item {
  id Int @id
}
"#
        .into(),
    );
    assert_single_error(
        &schema,
        &[
            "Invalid relation mode setting: \"prisma\"",
            "Supported values: \"foreignKeys\"",
        ],
    );
}

#[test]
fn views_are_rejected_even_with_long_mapped_database_names() {
    let long_database_name = "v".repeat(32);
    let cases = [
        ("ShortView", String::new(), String::new()),
        ("MappedView", String::new(), format!("@@map(\"{long_database_name}\")")),
        (
            "MappedColumnView",
            format!("@map(\"{long_database_name}\")"),
            String::new(),
        ),
    ];

    for (view_name, field_map, view_map) in cases {
        let schema = validate(&format!(
            r#"
generator client {{
  provider = "prisma-client"
  previewFeatures = ["views"]
}}

view {view_name} {{
  id Int @unique {field_map}
  {view_map}
}}
"#
        ));

        assert_single_error(&schema, &[view_name, "Views are not supported", "Firebird"]);
    }
}

#[test]
fn updateable_ids_retain_native_foreign_key_action_validation() {
    for action in ["Cascade", "NoAction", "SetNull", "SetDefault"] {
        assert_valid(&format!(
            r#"
model Parent {{
  code String @id @db.VarChar(6)
  children Child[]
}}

model Child {{
  id Int @id
  parentCode String? @default("DF") @db.VarChar(6)
  parent Parent? @relation(fields: [parentCode], references: [code], onUpdate: {action})
}}
"#
        ));
    }

    let schema = validate(
        r#"
model Parent {
  code String @id @db.VarChar(6)
  children Child[]
}

model Child {
  id Int @id
  parentCode String @db.VarChar(6)
  parent Parent @relation(fields: [parentCode], references: [code], onUpdate: Restrict)
}
"#,
    );
    assert_single_error(&schema, &["Invalid referential action", "Restrict", "Allowed values"]);
}
