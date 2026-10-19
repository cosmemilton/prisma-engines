// Added for experimental Firebird support (2026-10-18); see MODIFICATIONS.md.
//! Firebird 3 schema operations use an external JavaScript adapter, including in Wasm.

mod destructive_change_checker;
mod renderer;
mod schema_calculator;
mod schema_differ;

use super::{SqlConnector, SqlDialect, UsingExternalShadowDb, quaint_error_to_connector_error};
use crate::sql_renderer::SqlRenderer;
use psl::PreviewFeatures;
use quaint::{
    connector::{
        AdapterProvider, ExternalConnectionInfo, ExternalConnector, ExternalConnectorFactory, IsolationLevel,
        Transaction,
    },
    prelude::{ResultSet, Value},
    visitor::{Firebird, Visitor},
};
use schema_connector::{
    BoxFuture, ConnectorError, ConnectorResult, Namespaces, SchemaFilter, migrations_directory::Migrations,
};
use sql_schema_describer::{DescriberErrorKind, SqlSchema, firebird::FirebirdSchemaExt};
use std::sync::Arc;

const LOCK_TABLE: &str = "_prisma_migrate_lock";
const LOCK_TABLE_EXISTS: &str = r#"SELECT 1 AS "exists" FROM RDB$RELATIONS WHERE RDB$RELATION_NAME = '_prisma_migrate_lock' AND COALESCE(RDB$SYSTEM_FLAG, 0) = 0 AND RDB$VIEW_BLR IS NULL AND RDB$RELATION_TYPE = 0"#;
const LOCK_ROW_EXISTS: &str = r#"SELECT "id" FROM "_prisma_migrate_lock" WHERE "id" = 1"#;
// These objects are not represented by Prisma's shared SQL migration steps. Reject
// them before a reset rather than claiming that replay produced an empty database.
const UNMANAGED_RESET_OBJECTS: &str = r#"
SELECT CAST('sequence' AS VARCHAR(20)) AS "kind", TRIM(TRAILING FROM RDB$GENERATOR_NAME) AS "name"
FROM RDB$GENERATORS WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0
UNION ALL
SELECT 'trigger', TRIM(TRAILING FROM RDB$TRIGGER_NAME)
FROM RDB$TRIGGERS WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0
UNION ALL
SELECT 'domain', TRIM(TRAILING FROM RDB$FIELD_NAME)
FROM RDB$FIELDS WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0 AND RDB$FIELD_NAME NOT STARTING WITH 'RDB$'
UNION ALL
SELECT 'procedure', TRIM(TRAILING FROM RDB$PROCEDURE_NAME)
FROM RDB$PROCEDURES WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0
UNION ALL
SELECT 'function', TRIM(TRAILING FROM RDB$FUNCTION_NAME)
FROM RDB$FUNCTIONS WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0
UNION ALL
SELECT 'package', TRIM(TRAILING FROM RDB$PACKAGE_NAME)
FROM RDB$PACKAGES WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0
UNION ALL
SELECT 'exception', TRIM(TRAILING FROM RDB$EXCEPTION_NAME)
FROM RDB$EXCEPTIONS WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0
UNION ALL
SELECT 'role', TRIM(TRAILING FROM RDB$ROLE_NAME)
FROM RDB$ROLES WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0
UNION ALL
SELECT 'blob filter', TRIM(TRAILING FROM RDB$FUNCTION_NAME)
FROM RDB$FILTERS WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0
UNION ALL
SELECT 'collation', TRIM(TRAILING FROM RDB$COLLATION_NAME)
FROM RDB$COLLATIONS WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0
UNION ALL
SELECT 'character set', TRIM(TRAILING FROM RDB$CHARACTER_SET_NAME)
FROM RDB$CHARACTER_SETS WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0
UNION ALL
SELECT 'mapping', TRIM(TRAILING FROM RDB$MAP_NAME)
FROM RDB$AUTH_MAPPING WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0
"#;

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct FirebirdDialect;

impl SqlDialect for FirebirdDialect {
    fn renderer(&self) -> Box<dyn SqlRenderer> {
        Box::new(Self)
    }
    fn schema_differ(&self) -> Box<dyn crate::sql_schema_differ::SqlSchemaDifferFlavour> {
        Box::new(Self)
    }
    fn schema_calculator(&self) -> Box<dyn crate::sql_schema_calculator::SqlSchemaCalculatorFlavour> {
        Box::new(Self)
    }
    fn destructive_change_checker(
        &self,
    ) -> Box<dyn crate::sql_destructive_change_checker::DestructiveChangeCheckerFlavour> {
        Box::new(Self)
    }
    fn datamodel_connector(&self) -> &'static dyn psl::datamodel_connector::Connector {
        psl::builtin_connectors::FIREBIRD
    }

    fn check_schema_features(&self, schema: &psl::ValidatedSchema) -> ConnectorResult<()> {
        for model in schema.db.walk_models() {
            if matches!(model.database_name(), LOCK_TABLE | crate::MIGRATIONS_TABLE_NAME) {
                return Err(unsupported("the Prisma migration and lock table names are reserved"));
            }
        }
        Ok(())
    }

    fn empty_database_schema(&self) -> SqlSchema {
        let mut schema = SqlSchema::default();
        schema.set_connector_data(Box::new(FirebirdSchemaExt::default()));
        schema
    }

    #[cfg(any(
        feature = "mssql-native",
        feature = "mysql-native",
        feature = "postgresql-native",
        feature = "sqlite-native"
    ))]
    fn connect_to_shadow_db(
        &self,
        _url: String,
        _preview_features: PreviewFeatures,
    ) -> BoxFuture<'_, ConnectorResult<Box<dyn SqlConnector>>> {
        Box::pin(async {
            Err(unsupported(
                "shadow database connections require the schema-engine Wasm and the Firebird adapter factory",
            ))
        })
    }

    #[cfg(not(any(
        feature = "mssql-native",
        feature = "mysql-native",
        feature = "postgresql-native",
        feature = "sqlite-native"
    )))]
    fn connect_to_shadow_db(
        &self,
        factory: Arc<dyn ExternalConnectorFactory>,
    ) -> BoxFuture<'_, ConnectorResult<Box<dyn SqlConnector>>> {
        Box::pin(async move {
            let adapter = factory
                .connect_to_shadow_db()
                .await
                .ok_or_else(|| {
                    unsupported("a separately provisioned shadow database must be configured on the Firebird adapter")
                })?
                .map_err(convert_error)?;
            Ok(Box::new(FirebirdConnector::new_external(adapter).await?) as Box<dyn SqlConnector>)
        })
    }
}

pub(crate) struct FirebirdConnector {
    // Drop the transaction before the adapter as a backstop. dispose() awaits both in order.
    migration_lock: Option<Box<dyn Transaction + 'static>>,
    adapter: Arc<dyn ExternalConnector>,
    preview_features: PreviewFeatures,
    version: String,
}

impl std::fmt::Debug for FirebirdConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<Firebird 3 external connector>")
    }
}

impl FirebirdConnector {
    pub(crate) async fn new_external(adapter: Arc<dyn ExternalConnector>) -> ConnectorResult<Self> {
        let result = async {
            if adapter.provider() != AdapterProvider::Firebird {
                return Err(unsupported("a Firebird driver adapter is required"));
            }
            let version = adapter
                .version()
                .await
                .map_err(convert_error)?
                .ok_or_else(|| unsupported("the adapter did not report a database version"))?;
            if !version.starts_with("3.") {
                return Err(unsupported(format!(
                    "this milestone requires Firebird 3.x; database reported {version:?}"
                )));
            }
            Ok(version)
        }
        .await;
        match result {
            Ok(version) => Ok(Self {
                migration_lock: None,
                adapter,
                preview_features: PreviewFeatures::empty(),
                version,
            }),
            Err(error) => {
                let _ = adapter.dispose().await;
                Err(error)
            }
        }
    }

    async fn acquire_migration_lock(&mut self) -> ConnectorResult<()> {
        if self.migration_lock.is_some() {
            return Ok(());
        }
        let info = self.adapter.get_connection_info().await.map_err(convert_error)?;
        validate_migration_lock_connection_info(&info)?;
        // Bootstrap the dedicated lock relation in committed adapter operations. A concurrent
        // initializer is accepted only after the expected relation/row can actually be read.
        if self
            .adapter
            .query_raw(LOCK_TABLE_EXISTS, &[])
            .await
            .map_err(convert_error)?
            .is_empty()
        {
            let create = self
                .adapter
                .execute_script(r#"CREATE TABLE "_prisma_migrate_lock" ("id" INTEGER NOT NULL PRIMARY KEY)"#)
                .await;
            if let Err(error) = create {
                if self
                    .adapter
                    .query_raw(LOCK_TABLE_EXISTS, &[])
                    .await
                    .map_err(convert_error)?
                    .is_empty()
                {
                    return Err(convert_error(error));
                }
            }
        }
        let insert = self.adapter.execute_raw(
            r#"INSERT INTO "_prisma_migrate_lock" ("id") SELECT 1 FROM RDB$DATABASE WHERE NOT EXISTS (SELECT 1 FROM "_prisma_migrate_lock" WHERE "id" = 1)"#, &[]
        ).await;
        if let Err(error) = insert {
            if self
                .adapter
                .query_raw(LOCK_ROW_EXISTS, &[])
                .await
                .map_err(convert_error)?
                .is_empty()
            {
                return Err(convert_error(error));
            }
        }
        let transaction = self
            .adapter
            .start_owned_transaction(Some(IsolationLevel::ReadCommitted))
            .await
            .ok_or_else(|| unsupported("this adapter cannot own a transaction for the migration lock"))?
            .map_err(convert_error)?;
        let lock_result = transaction
            .query_raw(
                r#"SELECT "id" FROM "_prisma_migrate_lock" WHERE "id" = 1 WITH LOCK"#,
                &[],
            )
            .await;
        match lock_result {
            Ok(rows) if rows.len() == 1 => {
                self.migration_lock = Some(transaction);
                Ok(())
            }
            result => {
                let _ = transaction.rollback().await;
                match result {
                    Err(error) => Err(migration_lock_error(error)),
                    Ok(_) => Err(unsupported("the migration lock row is missing")),
                }
            }
        }
    }
}

impl SqlConnector for FirebirdConnector {
    fn dialect(&self) -> Box<dyn SqlDialect> {
        Box::new(FirebirdDialect)
    }
    fn shadow_db_url(&self) -> Option<&str> {
        None
    }
    fn connector_type(&self) -> &'static str {
        "firebird"
    }
    fn acquire_lock(&mut self) -> BoxFuture<'_, ConnectorResult<()>> {
        Box::pin(self.acquire_migration_lock())
    }
    fn create_database(&mut self) -> BoxFuture<'_, ConnectorResult<String>> {
        Box::pin(async { Err(unsupported("provision Firebird databases explicitly before connecting")) })
    }
    fn drop_database(&mut self) -> BoxFuture<'_, ConnectorResult<()>> {
        Box::pin(async { Err(unsupported("dropping a Firebird database is not implemented")) })
    }

    fn create_migrations_table(&mut self) -> BoxFuture<'_, ConnectorResult<()>> {
        self.raw_cmd(
            r#"CREATE TABLE "_prisma_migrations" (
            "id" VARCHAR(36) NOT NULL,
            "checksum" VARCHAR(64) NOT NULL,
            "finished_at" TIMESTAMP,
            "migration_name" VARCHAR(255) NOT NULL,
            "logs" BLOB SUB_TYPE TEXT CHARACTER SET UTF8,
            "rolled_back_at" TIMESTAMP,
            "started_at" TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL,
            "applied_steps_count" INTEGER DEFAULT 0 NOT NULL,
            CONSTRAINT "_prisma_migrations_pk" PRIMARY KEY ("id")
        )"#,
        )
    }
    fn drop_migrations_table(&mut self) -> BoxFuture<'_, ConnectorResult<()>> {
        self.raw_cmd(r#"DROP TABLE "_prisma_migrations""#)
    }

    fn describe_schema(&mut self, namespaces: Option<Namespaces>) -> BoxFuture<'_, ConnectorResult<SqlSchema>> {
        Box::pin(async move {
            validate_namespaces(namespaces)?;
            sql_schema_describer::firebird::SqlSchemaDescriber::new(self.adapter.as_ref())
                .describe_impl()
                .await
                .map_err(|error| match error.into_kind() {
                    DescriberErrorKind::QuaintError(error) => convert_error(error),
                    DescriberErrorKind::CrossSchemaReference { .. } => {
                        unsupported("cross-schema references are not supported")
                    }
                })
        })
    }

    fn table_names(
        &mut self,
        namespaces: Option<Namespaces>,
        filter: SchemaFilter,
    ) -> BoxFuture<'_, ConnectorResult<Vec<String>>> {
        Box::pin(async move {
            validate_namespaces(namespaces)?;
            let rows = self.adapter.query_raw(r#"SELECT TRIM(TRAILING FROM RDB$RELATION_NAME) AS "table_name" FROM RDB$RELATIONS WHERE COALESCE(RDB$SYSTEM_FLAG, 0) = 0 AND RDB$VIEW_BLR IS NULL ORDER BY RDB$RELATION_NAME"#, &[]).await.map_err(convert_error)?;
            Ok(rows
                .into_iter()
                .filter_map(|row| row.get("table_name").and_then(|value| value.to_string()))
                .filter(|name| {
                    name != LOCK_TABLE
                        && !self
                            .dialect()
                            .schema_differ()
                            .contains_table(&filter.external_tables, None, name)
                })
                .collect())
        })
    }

    fn ensure_connection_validity(&mut self) -> BoxFuture<'_, ConnectorResult<()>> {
        Box::pin(async move {
            self.adapter.version().await.map_err(convert_error)?;
            Ok(())
        })
    }
    fn describe_query<'a>(
        &'a mut self,
        _sql: &'a str,
    ) -> BoxFuture<'a, ConnectorResult<quaint::connector::DescribedQuery>> {
        Box::pin(async { Err(unsupported("TypedSQL query introspection is not implemented")) })
    }
    fn query<'a>(&'a mut self, query: quaint::ast::Query<'a>) -> BoxFuture<'a, ConnectorResult<ResultSet>> {
        Box::pin(async move {
            let (sql, params) = Firebird::build(query).map_err(convert_error)?;
            self.adapter.query_raw(&sql, &params).await.map_err(convert_error)
        })
    }
    fn query_raw<'a>(&'a mut self, sql: &'a str, params: &'a [Value<'a>]) -> BoxFuture<'a, ConnectorResult<ResultSet>> {
        Box::pin(async move { self.adapter.query_raw(sql, params).await.map_err(convert_error) })
    }
    fn raw_cmd<'a>(&'a mut self, sql: &'a str) -> BoxFuture<'a, ConnectorResult<()>> {
        Box::pin(async move { self.adapter.execute_script(sql).await.map_err(convert_error) })
    }
    fn apply_migration_script<'a>(
        &'a mut self,
        migration_name: &'a str,
        script: &'a str,
    ) -> BoxFuture<'a, ConnectorResult<()>> {
        Box::pin(async move {
            self.adapter.execute_script(script).await.map_err(|error| {
                ConnectorError::from_source_with_context(
                    error,
                    format!("Failed to apply Firebird migration `{migration_name}`").into_boxed_str(),
                )
            })
        })
    }
    fn reset(&mut self, _namespaces: Option<Namespaces>) -> BoxFuture<'_, ConnectorResult<()>> {
        // The common connector implements a filtered schema reset after this explicit failure.
        Box::pin(async {
            Err(unsupported(
                "database recreation is unavailable; use the filtered schema reset",
            ))
        })
    }

    fn validate_reset(&mut self) -> BoxFuture<'_, ConnectorResult<()>> {
        Box::pin(async move {
            let objects = self
                .adapter
                .query_raw(UNMANAGED_RESET_OBJECTS, &[])
                .await
                .map_err(convert_error)?;
            validate_reset_objects(objects)
        })
    }
    fn sql_schema_from_migration_history<'a>(
        &'a mut self,
        migrations: &'a Migrations,
        namespaces: Option<Namespaces>,
        filter: &'a SchemaFilter,
        external_shadow_db: UsingExternalShadowDb,
    ) -> BoxFuture<'a, ConnectorResult<SqlSchema>> {
        Box::pin(async move {
            if !matches!(external_shadow_db, UsingExternalShadowDb::Yes) {
                return Err(unsupported("a separately provisioned shadow database is required"));
            }
            validate_namespaces(namespaces.clone())?;
            self.validate_reset().await?;
            self.acquire_migration_lock().await?;
            crate::best_effort_reset(self, namespaces.clone(), filter).await?;
            if !migrations.shadow_db_init_script.trim().is_empty() {
                self.raw_cmd(&migrations.shadow_db_init_script).await?;
            }
            for migration in &migrations.migration_directories {
                let script = migration.read_migration_script()?;
                self.apply_migration_script(migration.migration_name(), &script)
                    .await
                    .map_err(|error| {
                        error.into_migration_does_not_apply_cleanly(migration.migration_name().to_owned())
                    })?;
            }
            self.describe_schema(namespaces).await
        })
    }
    fn set_preview_features(&mut self, features: PreviewFeatures) {
        self.preview_features = features;
    }
    fn preview_features(&self) -> PreviewFeatures {
        self.preview_features
    }
    fn version(&mut self) -> BoxFuture<'_, ConnectorResult<Option<String>>> {
        Box::pin(async move { Ok(Some(self.version.clone())) })
    }
    fn search_path(&self) -> &str {
        ""
    }
    fn default_namespace(&self) -> Option<&str> {
        None
    }
    fn dispose(&mut self) -> BoxFuture<'_, ConnectorResult<()>> {
        Box::pin(async move {
            let release = match self.migration_lock.take() {
                Some(transaction) => transaction.rollback().await.map_err(convert_error),
                None => Ok(()),
            };
            let dispose = self.adapter.dispose().await.map_err(convert_error);
            release.and(dispose)
        })
    }
}

fn validate_namespaces(namespaces: Option<Namespaces>) -> ConnectorResult<()> {
    if namespaces.is_some_and(|namespaces| namespaces.into_iter().any(|name| !name.is_empty())) {
        return Err(unsupported("schemas/namespaces are not supported"));
    }
    Ok(())
}
fn convert_error(error: quaint::error::Error) -> ConnectorError {
    quaint_error_to_connector_error(error, None)
}
fn validate_migration_lock_connection_info(info: &ExternalConnectionInfo) -> ConnectorResult<()> {
    if !info.max_concurrent_connections.is_some_and(|count| count >= 2) {
        return Err(unsupported(
            "migration locking requires adapter maxConcurrentConnections >= 2; configure poolSize at least 2",
        ));
    }
    if info.supports_bounded_transaction_wait != Some(true) {
        return Err(unsupported(
            "migration locking requires supportsBoundedTransactionWait; configure transactionLockTimeout (0 means NOWAIT)",
        ));
    }
    Ok(())
}
fn validate_reset_objects(objects: ResultSet) -> ConnectorResult<()> {
    if let Some(object) = objects.first() {
        let kind = object
            .get("kind")
            .and_then(|value| value.to_string())
            .unwrap_or_else(|| "object".to_owned());
        let name = object
            .get("name")
            .and_then(|value| value.to_string())
            .unwrap_or_default();
        return Err(unsupported(format!(
            "reset and migration shadow replay cannot manage {kind} {name:?}; use a separately provisioned empty shadow database and migrations within the supported Firebird schema subset"
        )));
    }
    Ok(())
}
fn migration_lock_error(error: quaint::error::Error) -> ConnectorError {
    let mut context =
        "Could not acquire the Firebird migration lock. Another schema engine may be using this database.".to_owned();
    // Quaint Display renders the mapped kind, so put the actual driver diagnostics
    // in the visible context as well as retaining the original error as the source.
    if let Some(code) = error.original_code() {
        context.push_str(&format!(" Driver error code: {code}."));
    }
    if let Some(message) = error.original_message() {
        context.push(' ');
        context.push_str(message);
    }
    ConnectorError::from_source_with_context(error, context.into_boxed_str())
}
fn unsupported(message: impl std::fmt::Display) -> ConnectorError {
    ConnectorError::from_msg(format!("Firebird schema engine: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_schema_retains_firebird_metadata_for_the_differ() {
        let schema = FirebirdDialect.empty_database_schema();
        let metadata: &FirebirdSchemaExt = schema.downcast_connector_data();
        assert!(metadata.identities.is_empty());
        assert!(metadata.indexes.is_empty());
    }

    #[test]
    fn database_names_used_for_migration_state_are_reserved() {
        for name in [LOCK_TABLE, crate::MIGRATIONS_TABLE_NAME] {
            let schema = format!(
                "datasource db {{\n provider = \"firebird\"\n}}\nmodel Lock {{\n id Int @id\n @@map(\"{name}\")\n}}\n"
            );
            let schema = psl::parse_schema_without_extensions(schema).unwrap();
            assert!(
                FirebirdDialect
                    .check_schema_features(&schema)
                    .unwrap_err()
                    .to_string()
                    .contains("reserved")
            );
        }
    }

    #[test]
    fn nonempty_namespaces_are_rejected() {
        let namespaces = Namespaces::from_vec(&mut vec!["schema".to_owned()]);
        assert!(validate_namespaces(namespaces).is_err());
        assert!(validate_namespaces(None).is_ok());
    }

    #[test]
    fn migration_lock_rejects_missing_or_unsafe_pool_metadata_before_sql() {
        let mut info = ExternalConnectionInfo::new(quaint::connector::SqlFamily::Firebird, None, None, false);
        assert!(
            validate_migration_lock_connection_info(&info)
                .unwrap_err()
                .to_string()
                .contains("maxConcurrentConnections")
        );
        info.max_concurrent_connections = Some(1);
        info.supports_bounded_transaction_wait = Some(true);
        assert!(validate_migration_lock_connection_info(&info).is_err());
        info.max_concurrent_connections = Some(2);
        for bounded in [None, Some(false)] {
            info.supports_bounded_transaction_wait = bounded;
            assert!(
                validate_migration_lock_connection_info(&info)
                    .unwrap_err()
                    .to_string()
                    .contains("supportsBoundedTransactionWait")
            );
        }
        info.supports_bounded_transaction_wait = Some(true);
        assert!(validate_migration_lock_connection_info(&info).is_ok());
    }

    #[test]
    fn reset_rejects_unmanaged_catalog_objects_with_their_names() {
        for kind in ["sequence", "trigger", "domain", "procedure"] {
            let objects = ResultSet::new(
                vec!["kind".to_owned(), "name".to_owned()],
                vec![],
                vec![vec![Value::text(kind), Value::text("manual_object")]],
            );
            let error = validate_reset_objects(objects).unwrap_err().to_string();
            assert!(error.contains(kind));
            assert!(error.contains("manual_object"));
        }
        assert!(validate_reset_objects(ResultSet::default()).is_ok());
    }

    #[test]
    fn firebird_migration_lock_error_preserves_actual_driver_diagnostics() {
        let message = "lock conflict on no wait transaction; SQLSTATE = 40001";
        let mut builder = quaint::error::Error::builder(quaint::error::ErrorKind::TransactionWriteConflict);
        builder.set_original_code("335544345");
        builder.set_original_message(message);
        let error = migration_lock_error(builder.build()).to_string();
        assert!(error.contains("Firebird migration lock"));
        assert!(error.contains("335544345"));
        assert!(error.contains(message));

        let error = migration_lock_error(
            quaint::error::Error::builder(quaint::error::ErrorKind::TransactionWriteConflict).build(),
        )
        .to_string();
        assert!(error.contains("Firebird migration lock"));
        assert!(!error.contains("Driver error code:"));
        assert!(!error.contains("SQLSTATE"));
    }
}
