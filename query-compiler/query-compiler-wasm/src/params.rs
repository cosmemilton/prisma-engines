// Modified for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
use quaint::prelude::{ExternalConnectionInfo, SqlFamily};
use serde::Deserialize;

// TODO: the code below largely duplicates driver_adapters::types, we should ideally use that
// crate instead, but it currently uses #cfg target a lot, which causes build issues when not
// explicitly building against wasm.

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsConnectionInfo {
    pub schema_name: Option<String>,
    pub max_bind_values: Option<u32>,
    pub supports_relation_joins: bool,
}

impl JsConnectionInfo {
    pub fn into_external_connection_info(self, provider: AdapterProvider) -> ExternalConnectionInfo {
        ExternalConnectionInfo::new(
            SqlFamily::from(provider),
            self.schema_name(provider).map(ToOwned::to_owned),
            self.max_bind_values.map(|v| v as usize),
            self.supports_relation_joins && provider.supports_relation_joins(),
        )
    }

    fn schema_name(&self, provider: AdapterProvider) -> Option<&str> {
        // Firebird has no SQL schema namespace. Adapter metadata must never add
        // PostgreSQL's `public` (or any other qualifier) to Firebird tables.
        #[cfg(feature = "firebird")]
        if provider == AdapterProvider::Firebird {
            return None;
        }

        self.schema_name
            .as_deref()
            .or_else(|| self.default_schema_name(provider))
    }

    fn default_schema_name(&self, provider: AdapterProvider) -> Option<&str> {
        match provider {
            #[cfg(feature = "mysql")]
            AdapterProvider::Mysql => None,
            #[cfg(feature = "postgresql")]
            AdapterProvider::Postgres => Some(quaint::connector::DEFAULT_POSTGRES_SCHEMA),
            #[cfg(feature = "sqlite")]
            AdapterProvider::Sqlite => Some(quaint::connector::DEFAULT_SQLITE_DATABASE),
            #[cfg(feature = "mssql")]
            AdapterProvider::SqlServer => Some(quaint::connector::DEFAULT_MSSQL_SCHEMA),
            #[cfg(feature = "firebird")]
            AdapterProvider::Firebird => None,
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AdapterProvider {
    #[cfg(feature = "mysql")]
    Mysql,
    #[cfg(feature = "postgresql")]
    Postgres,
    #[cfg(feature = "sqlite")]
    Sqlite,
    #[cfg(feature = "mssql")]
    #[serde(rename = "sqlserver")]
    SqlServer,
    #[cfg(feature = "firebird")]
    Firebird,
}

impl AdapterProvider {
    fn supports_relation_joins(self) -> bool {
        // The JSON/lateral join strategy is not implemented for Firebird 3.
        #[cfg(feature = "firebird")]
        if self == Self::Firebird {
            return false;
        }
        true
    }
}

impl From<AdapterProvider> for SqlFamily {
    fn from(f: AdapterProvider) -> Self {
        match f {
            #[cfg(feature = "mysql")]
            AdapterProvider::Mysql => SqlFamily::Mysql,
            #[cfg(feature = "postgresql")]
            AdapterProvider::Postgres => SqlFamily::Postgres,
            #[cfg(feature = "sqlite")]
            AdapterProvider::Sqlite => SqlFamily::Sqlite,
            #[cfg(feature = "mssql")]
            AdapterProvider::SqlServer => SqlFamily::Mssql,
            #[cfg(feature = "firebird")]
            AdapterProvider::Firebird => SqlFamily::Firebird,
        }
    }
}

#[cfg(all(test, feature = "firebird"))]
mod tests {
    use super::*;

    #[test]
    fn firebird_provider_is_distinct_and_has_no_default_schema() {
        let provider = serde_json::from_str::<AdapterProvider>("\"firebird\"").unwrap();
        assert_eq!(provider, AdapterProvider::Firebird);
        assert_eq!(SqlFamily::from(provider), SqlFamily::Firebird);
        assert_eq!(JsConnectionInfo::default().schema_name(provider), None);
    }

    #[test]
    fn firebird_ignores_schema_and_relation_join_metadata() {
        let info = JsConnectionInfo {
            schema_name: Some("public".into()),
            max_bind_values: Some(32767),
            supports_relation_joins: true,
        };
        assert_eq!(info.schema_name(AdapterProvider::Firebird), None);
        assert!(!AdapterProvider::Firebird.supports_relation_joins());
    }
}
