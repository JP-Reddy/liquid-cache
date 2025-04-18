use std::{any::Any, sync::Arc};

use arrow_schema::{DataType, Field, FieldRef, Schema, SchemaRef};
use async_trait::async_trait;
use datafusion::{
    catalog::Session,
    common::{GetExt, Statistics, internal_err, parsers::CompressionTypeVariant},
    config::TableParquetOptions,
    datasource::{
        file_format::{
            FileFormat, FilePushdownSupport, file_compression_type::FileCompressionType,
        },
        physical_plan::{FileScanConfig, FileSource},
    },
    error::Result,
    physical_optimizer::pruning::PruningPredicate,
    physical_plan::{ExecutionPlan, PhysicalExpr, metrics::ExecutionPlanMetricsSet},
    prelude::*,
};
#[cfg(test)]
pub(crate) use exec::CachedMetaReaderFactory;
use exec::LiquidParquetExec;
pub(crate) use exec::ParquetMetadataCacheReader;
use log::{debug, info};
use object_store::{ObjectMeta, ObjectStore};
use page_filter::PagePruningAccessPlanFilter;

use crate::{LiquidCacheMode, LiquidCacheRef};

// This is entirely copied from DataFusion
// We should make DataFusion to public this
mod page_filter;

// This is entirely copied from DataFusion
// We should make DataFusion to public this
mod row_filter;

// This is entirely copied from DataFusion
// We should make DataFusion to public this
mod row_group_filter;

mod opener;

mod exec;

#[derive(Debug)]
pub struct LiquidParquetFactory {}

impl GetExt for LiquidParquetFactory {
    fn get_ext(&self) -> String {
        "parquet".to_string()
    }
}

/// A file format for liquid parquet.
#[derive(Debug)]
pub struct LiquidParquetFileFormat {
    options: TableParquetOptions,
    inner: Arc<dyn FileFormat>,   // is actually ParquetFormat
    liquid_cache: LiquidCacheRef, // a file format deals with multiple files
    liquid_cache_mode: LiquidCacheMode,
}

impl LiquidParquetFileFormat {
    /// Creates a new liquid parquet file format.
    pub fn new(
        options: TableParquetOptions,
        inner: Arc<dyn FileFormat>,
        liquid_cache: LiquidCacheRef,
        liquid_cache_mode: LiquidCacheMode,
    ) -> Self {
        Self {
            options,
            liquid_cache,
            liquid_cache_mode,
            inner,
        }
    }
}

#[async_trait]
impl FileFormat for LiquidParquetFileFormat {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_ext(&self) -> String {
        LiquidParquetFactory {}.get_ext()
    }

    fn get_ext_with_compression(
        &self,
        file_compression_type: &FileCompressionType,
    ) -> Result<String> {
        let ext = self.get_ext();
        match file_compression_type.get_variant() {
            CompressionTypeVariant::UNCOMPRESSED => Ok(ext),
            _ => internal_err!("Unsupported compression type: {:?}", file_compression_type),
        }
    }

    async fn infer_schema(
        &self,
        state: &dyn Session,
        store: &Arc<dyn ObjectStore>,
        objects: &[ObjectMeta],
    ) -> Result<SchemaRef> {
        let parquet_schema = self.inner.infer_schema(state, store, objects).await?;
        let mut transformed = coerce_binary_to_string(&parquet_schema);
        if matches!(
            self.liquid_cache_mode,
            LiquidCacheMode::InMemoryLiquid { .. }
        ) {
            transformed = coerce_to_liquid_cache_types(&transformed);
        }
        Ok(Arc::new(transformed))
    }

    async fn infer_stats(
        &self,
        state: &dyn Session,
        store: &Arc<dyn ObjectStore>,
        table_schema: SchemaRef,
        object: &ObjectMeta,
    ) -> Result<Statistics> {
        self.inner
            .infer_stats(state, store, table_schema, object)
            .await
    }

    async fn create_physical_plan(
        &self,
        _state: &dyn Session,
        conf: FileScanConfig,
        filters: Option<&Arc<dyn PhysicalExpr>>,
    ) -> Result<Arc<dyn ExecutionPlan>> {
        info!("create_physical_plan for liquid parquet");

        let metrics = ExecutionPlanMetricsSet::new();

        let file_schema = &conf.file_schema;
        let pruning_predicate = filters
            .and_then(|predicate_expr| {
                match PruningPredicate::try_new(Arc::clone(predicate_expr), Arc::clone(file_schema))
                {
                    Ok(pruning_predicate) => Some(Arc::new(pruning_predicate)),
                    Err(e) => {
                        debug!("Could not create pruning predicate for: {e}");
                        None
                    }
                }
            })
            .filter(|p| !p.always_true());

        let page_pruning_predicate = filters
            .as_ref()
            .map(|predicate_expr| {
                PagePruningAccessPlanFilter::new(predicate_expr, Arc::clone(file_schema))
            })
            .map(Arc::new);

        let (projected_schema, _, projected_statistics, projected_output_ordering) = conf.project();

        let cache = LiquidParquetExec::compute_properties(
            projected_schema,
            &projected_output_ordering,
            &conf,
        );

        let exec = LiquidParquetExec {
            base_config: conf,
            table_parquet_options: self.options.clone(),
            predicate: filters.cloned(),
            cache,
            projected_statistics,
            metrics,
            pruning_predicate,
            page_pruning_predicate,
            liquid_cache: self.liquid_cache.clone(),
            liquid_cache_mode: self.liquid_cache_mode,
        };
        Ok(Arc::new(exec))
    }

    fn supports_filters_pushdown(
        &self,
        file_schema: &Schema,
        table_schema: &Schema,
        filters: &[&Expr],
    ) -> Result<FilePushdownSupport> {
        self.inner
            .supports_filters_pushdown(file_schema, table_schema, filters)
    }

    fn file_source(&self) -> Arc<dyn FileSource> {
        // this can be anything, as we don't really use it.
        self.inner.file_source()
    }
}

/// Create a new field with the specified data type, copying the other
/// properties from the input field
fn field_with_new_type(field: &FieldRef, new_type: DataType) -> FieldRef {
    Arc::new(field.as_ref().clone().with_data_type(new_type))
}

pub(crate) fn coerce_binary_to_string(schema: &Schema) -> Schema {
    let transformed_fields: Vec<Arc<Field>> = schema
        .fields
        .iter()
        .map(|field| match field.data_type() {
            DataType::Binary | DataType::LargeBinary | DataType::BinaryView => {
                field_with_new_type(field, DataType::Utf8)
            }
            _ => field.clone(),
        })
        .collect();
    Schema::new_with_metadata(transformed_fields, schema.metadata.clone())
}

pub(crate) fn coerce_string_to_view(schema: &Schema) -> Schema {
    let transformed_fields: Vec<Arc<Field>> = schema
        .fields
        .iter()
        .map(|field| match field.data_type() {
            DataType::Utf8 | DataType::LargeUtf8 => field_with_new_type(field, DataType::Utf8View),
            _ => field.clone(),
        })
        .collect();
    Schema::new_with_metadata(transformed_fields, schema.metadata.clone())
}

pub(crate) fn coerce_to_liquid_cache_types(schema: &Schema) -> Schema {
    let transformed_fields: Vec<Arc<Field>> = schema
        .fields
        .iter()
        .map(|field| match field.data_type() {
            DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => field_with_new_type(
                field,
                DataType::Dictionary(Box::new(DataType::UInt16), Box::new(DataType::Utf8)),
            ),
            DataType::Binary | DataType::LargeBinary | DataType::BinaryView => field_with_new_type(
                field,
                DataType::Dictionary(Box::new(DataType::UInt16), Box::new(DataType::Utf8)),
            ),
            _ => field.clone(),
        })
        .collect();
    Schema::new_with_metadata(transformed_fields, schema.metadata.clone())
}

/// Liquid cache reads binary as strings.
pub(crate) fn coerce_from_reader_to_liquid_types(schema: &Schema) -> Schema {
    let transformed_fields: Vec<Arc<Field>> = schema
        .fields
        .iter()
        .map(|field| {
            if field.data_type().equals_datatype(&DataType::Utf8View) {
                field_with_new_type(
                    field,
                    DataType::Dictionary(Box::new(DataType::UInt16), Box::new(DataType::Utf8)),
                )
            } else {
                field.clone()
            }
        })
        .collect();
    Schema::new_with_metadata(transformed_fields, schema.metadata.clone())
}
