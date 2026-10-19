// Modified for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
//! Schema validation for a Firebird 3.0 baseline using UTF8 character columns.
//!
//! Query compilation uses a dedicated Firebird visitor and a driver adapter.
//! Views, introspection and migrations remain outside this connector's runtime support.

mod native_types;

use std::borrow::Cow;

use crate::{
    ValidatedSchema,
    datamodel_connector::{
        Connector, ConnectorCapabilities, ConnectorCapability, ConstraintScope, Flavour, NativeTypeConstructor,
        NativeTypeInstance, RelationMode, walker_ext_traits::ScalarFieldWalkerExt,
    },
    diagnostics::{DatamodelError, Diagnostics, Span},
    parser_database::{ExtensionTypes, ReferentialAction, ScalarFieldType, ScalarType, ast::WithSpan, walkers},
};
use enumflags2::BitFlags;
pub use native_types::FirebirdType;

pub const CAPABILITIES: ConnectorCapabilities = enumflags2::make_bitflags!(ConnectorCapability::{
    AnyId |
    AutoIncrement |
    CompoundIds |
    CreateMany |
    CreateManyWriteableAutoIncId |
    DecimalType |
    InsertReturning |
    NamedPrimaryKeys |
    NamedForeignKeys |
    UpdateableId |
    SupportsTxIsolationReadCommitted |
    SupportsTxIsolationRepeatableRead |
    SupportsTxIsolationSnapshot |
    SupportsTxIsolationSerializable
});

const CONSTRAINT_SCOPES: &[ConstraintScope] = &[ConstraintScope::GlobalPrimaryKeyKeyIndexForeignKey];

// Firebird 3 limits CHAR and VARCHAR to 32767 and 32765 bytes respectively.
// UTF8 uses up to four bytes per character; keep the PSL length safe for both.
const MAX_UTF8_CHARACTERS: u32 = 8191;

pub struct FirebirdDatamodelConnector;

fn validate_identifier_characters(name: &str, object: &str, span: Span, errors: &mut Diagnostics) {
    // Firebird 3 stores identifiers in UNICODE_FSS and ignores trailing ASCII spaces.
    // Reject names that cannot be represented or could alias a different accepted name.
    let reason = if name.chars().any(|character| character > '\u{ffff}') {
        "must contain only Unicode BMP characters (at most U+FFFF)"
    } else if name.ends_with(' ') {
        "must not end with an ASCII space"
    } else {
        return;
    };

    errors.push_error(DatamodelError::new_validation_error(
        &format!("The database {object} name `{name}` {reason} for Firebird 3.0."),
        span,
    ));
}

impl Connector for FirebirdDatamodelConnector {
    fn provider_name(&self) -> &'static str {
        "firebird"
    }

    fn name(&self) -> &str {
        "Firebird"
    }

    fn flavour(&self) -> Flavour {
        Flavour::Firebird
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        CAPABILITIES
    }

    fn max_identifier_length(&self) -> usize {
        31
    }

    fn allowed_relation_mode_settings(&self) -> BitFlags<RelationMode> {
        RelationMode::ForeignKeys.into()
    }

    fn foreign_key_referential_actions(&self) -> BitFlags<ReferentialAction> {
        use ReferentialAction::*;

        NoAction | Cascade | SetNull | SetDefault
    }

    fn constraint_violation_scopes(&self) -> &'static [ConstraintScope] {
        CONSTRAINT_SCOPES
    }

    fn available_native_type_constructors(&self) -> &'static [NativeTypeConstructor] {
        native_types::CONSTRUCTORS
    }

    fn parse_native_type(
        &self,
        name: &str,
        args: &[String],
        span: Span,
        diagnostics: &mut Diagnostics,
    ) -> Option<NativeTypeInstance> {
        match FirebirdType::from_parts(name, args) {
            Ok(native_type) => Some(NativeTypeInstance::new(native_type)),
            Err(error) => {
                diagnostics.push_error(error.into_datamodel_error(span));
                None
            }
        }
    }

    fn native_type_to_parts<'t>(&self, native_type: &'t NativeTypeInstance) -> (&'t str, Cow<'t, [String]>) {
        native_type.downcast_ref::<FirebirdType>().to_parts()
    }

    fn scalar_type_for_native_type(
        &self,
        native_type: &NativeTypeInstance,
        _extension_types: &dyn ExtensionTypes,
    ) -> Option<ScalarFieldType> {
        let scalar = match native_type.downcast_ref::<FirebirdType>() {
            FirebirdType::SmallInt | FirebirdType::Integer => ScalarType::Int,
            FirebirdType::BigInt => ScalarType::BigInt,
            FirebirdType::Float | FirebirdType::DoublePrecision => ScalarType::Float,
            FirebirdType::Decimal(_) | FirebirdType::Numeric(_) => ScalarType::Decimal,
            FirebirdType::Boolean => ScalarType::Boolean,
            FirebirdType::Char(_) | FirebirdType::VarChar(_) | FirebirdType::Text => ScalarType::String,
            FirebirdType::Blob => ScalarType::Bytes,
            FirebirdType::Date | FirebirdType::Time | FirebirdType::Timestamp => ScalarType::DateTime,
        };

        Some(ScalarFieldType::BuiltInScalar(scalar))
    }

    fn default_native_type_for_scalar_type(
        &self,
        scalar_type: &ScalarFieldType,
        _schema: &ValidatedSchema,
    ) -> Option<NativeTypeInstance> {
        let native_type = match scalar_type.as_builtin_scalar()? {
            ScalarType::Int => FirebirdType::Integer,
            ScalarType::BigInt => FirebirdType::BigInt,
            ScalarType::Float => FirebirdType::DoublePrecision,
            ScalarType::Decimal => FirebirdType::Decimal(Some((18, 6))),
            ScalarType::Boolean => FirebirdType::Boolean,
            ScalarType::String => FirebirdType::VarChar(191),
            ScalarType::DateTime => FirebirdType::Timestamp,
            ScalarType::Bytes => FirebirdType::Blob,
            ScalarType::Json => return None,
        };

        Some(NativeTypeInstance::new(native_type))
    }

    fn validate_native_type_arguments(
        &self,
        native_type: &NativeTypeInstance,
        _scalar_type: Option<ScalarType>,
        span: Span,
        diagnostics: &mut Diagnostics,
    ) {
        let error = self.native_instance_error(native_type);

        match native_type.downcast_ref::<FirebirdType>() {
            FirebirdType::Decimal(Some((precision, _))) | FirebirdType::Numeric(Some((precision, _)))
                if !(1..=18).contains(precision) =>
            {
                diagnostics.push_error(
                    error.new_argument_m_out_of_range_error("Precision can range from 1 to 18 in Firebird 3.0.", span),
                );
            }
            FirebirdType::Decimal(Some((precision, scale))) | FirebirdType::Numeric(Some((precision, scale)))
                if scale > precision =>
            {
                diagnostics.push_error(error.new_scale_larger_than_precision_error(span));
            }
            FirebirdType::Char(length) | FirebirdType::VarChar(length)
                if !(1..=MAX_UTF8_CHARACTERS).contains(length) =>
            {
                diagnostics.push_error(error.new_argument_m_out_of_range_error(
                    "Length can range from 1 to 8191 characters with the UTF8 character set in Firebird 3.0.",
                    span,
                ));
            }
            _ => (),
        }
    }

    fn validate_view(&self, view: walkers::ModelWalker<'_>, errors: &mut Diagnostics) {
        errors.push_error(DatamodelError::new_model_validation_error(
            "Views are not supported by the experimental Firebird connector.",
            "view",
            view.name(),
            view.ast_model().span(),
        ));
    }

    fn validate_model(&self, model: walkers::ModelWalker<'_>, _relation_mode: RelationMode, errors: &mut Diagnostics) {
        let limit = self.max_identifier_length();
        validate_identifier_characters(model.database_name(), "table", model.ast_model().span(), errors);
        if model.database_name().len() > limit {
            errors.push_error(DatamodelError::new_model_validation_error(
                &format!(
                    "The database table name `{}` must not exceed {limit} bytes for Firebird 3.0.",
                    model.database_name()
                ),
                "model",
                model.name(),
                model.ast_model().span(),
            ));
        }

        for field in model.scalar_fields() {
            validate_identifier_characters(field.database_name(), "column", field.ast_field().span(), errors);
            if field.database_name().len() > limit {
                errors.push_error(DatamodelError::new_field_validation_error(
                    &format!(
                        "The database column name `{}` must not exceed {limit} bytes for Firebird 3.0.",
                        field.database_name()
                    ),
                    "model",
                    model.name(),
                    field.name(),
                    field.ast_field().span(),
                ));
            }
        }

        for index in model.indexes() {
            if let Some(name) = index.mapped_name() {
                validate_identifier_characters(name, "constraint", index.ast_attribute().span, errors);
            }

            for field in index.scalar_field_attributes() {
                let field = field.as_index_field();
                let native_type = field.native_type_instance(self).or_else(|| {
                    matches!(
                        field.scalar_field_type(),
                        ScalarFieldType::BuiltInScalar(ScalarType::Bytes)
                    )
                    .then(|| NativeTypeInstance::new(FirebirdType::Blob))
                });
                let Some(native_type) = native_type else {
                    continue;
                };

                if !matches!(
                    native_type.downcast_ref::<FirebirdType>(),
                    FirebirdType::Text | FirebirdType::Blob
                ) {
                    continue;
                }

                let error = self.native_instance_error(&native_type);
                errors.push_error(if index.is_unique() {
                    error.new_incompatible_native_type_with_unique("", index.ast_attribute().span)
                } else {
                    error.new_incompatible_native_type_with_index("", index.ast_attribute().span)
                });
                break;
            }
        }

        if let Some(primary_key) = model.primary_key() {
            if let Some(name) = primary_key.mapped_name() {
                validate_identifier_characters(name, "constraint", primary_key.ast_attribute().span, errors);
            }

            for field in primary_key.scalar_field_attributes() {
                let field = field.as_index_field();
                let native_type = field.native_type_instance(self).or_else(|| {
                    matches!(
                        field.scalar_field_type(),
                        ScalarFieldType::BuiltInScalar(ScalarType::Bytes)
                    )
                    .then(|| NativeTypeInstance::new(FirebirdType::Blob))
                });
                let Some(native_type) = native_type else {
                    continue;
                };

                if matches!(
                    native_type.downcast_ref::<FirebirdType>(),
                    FirebirdType::Text | FirebirdType::Blob
                ) {
                    errors.push_error(
                        self.native_instance_error(&native_type)
                            .new_incompatible_native_type_with_id("", primary_key.ast_attribute().span),
                    );
                    break;
                }
            }
        }

        for field in model.relation_fields() {
            if let Some(name) = field.mapped_name() {
                validate_identifier_characters(name, "constraint", field.ast_field().span(), errors);
            }
        }
    }

    fn validate_url(&self, url: &str) -> Result<(), String> {
        if url.starts_with("firebird://") {
            Ok(())
        } else {
            Err("must start with the protocol `firebird://`.".to_owned())
        }
    }
}
