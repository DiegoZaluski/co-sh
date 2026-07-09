use arrow_array::{Array, FixedSizeListArray, Float32Array, RecordBatch, StringArray};
use arrow_schema::{ArrowError, DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub content: String,
}

#[derive(Debug, Error)]
pub enum VecDbError {
    #[error("Invalid id: {0}")]
    InvalidId(String),
    #[error("vector dimension must be {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
    #[error("Entry not found: {0}")]
    NotFound(String),
    #[error("Database error: {0}")]
    Database(String),
}

impl From<lancedb::error::Error> for VecDbError {
    fn from(e: lancedb::error::Error) -> Self {
        Self::Database(e.to_string())
    }
}

impl From<ArrowError> for VecDbError {
    fn from(e: ArrowError) -> Self {
        Self::Database(e.to_string())
    }
}

fn validate_id(id: &str) -> Result<(), VecDbError> {
    if id.is_empty() || id.len() > 100 {
        return Err(VecDbError::InvalidId(
            "must be non-empty and <= 100 characters".into(),
        ));
    }
    if id.contains('\'') || id.contains(';') || id.contains('"') {
        return Err(VecDbError::InvalidId(
            "contains forbidden characters".into(),
        ));
    }
    Ok(())
}

fn schema(vector_dim: i32) -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                vector_dim,
            ),
            false,
        ),
    ]))
}

fn build_batch(
    id: &str,
    content: &str,
    vector: &[f32],
    vector_dim: i32,
) -> Result<RecordBatch, VecDbError> {
    let id_array = StringArray::from(vec![id]);
    let content_array = StringArray::from(vec![content]);
    let values = Float32Array::from(vector.to_vec());
    let vector_array = FixedSizeListArray::new(
        Arc::new(Field::new("item", DataType::Float32, true)),
        vector_dim,
        Arc::new(values),
        None,
    );

    Ok(RecordBatch::try_new(
        schema(vector_dim),
        vec![
            Arc::new(id_array),
            Arc::new(content_array),
            Arc::new(vector_array),
        ],
    )?)
}

pub struct VecDb {
    table: lancedb::Table,
    vector_dim: usize,
}

impl VecDb {
    pub async fn connect(
        uri: &str,
        table_name: &str,
        vector_dim: usize,
    ) -> Result<Self, VecDbError> {
        let connection = lancedb::connect(uri).execute().await?;

        let table = if let Ok(table) = connection.open_table(table_name).execute().await {
            table
        } else {
            let dim = i32::try_from(vector_dim)
                .map_err(|_| VecDbError::Database("vector dimension exceeds i32 range".into()))?;
            connection
                .create_empty_table(table_name, schema(dim))
                .execute()
                .await?
        };

        Ok(Self { table, vector_dim })
    }

    pub async fn post(
        &self,
        id: &str,
        content: &str,
        vector: Vec<f32>,
    ) -> Result<String, VecDbError> {
        if vector.len() != self.vector_dim {
            return Err(VecDbError::DimensionMismatch {
                expected: self.vector_dim,
                actual: vector.len(),
            });
        }
        validate_id(id)?;

        if let Some(existing_id) = self.find_id_by_content(content).await? {
            return Ok(existing_id);
        }

        let dim = i32::try_from(self.vector_dim)
            .map_err(|_| VecDbError::Database("vector dimension exceeds i32 range".into()))?;
        let batch = build_batch(id, content, &vector, dim)?;
        self.table.add(vec![batch]).execute().await?;

        Ok(id.to_string())
    }

    pub async fn get(&self, query_vector: &[f32], limit: usize) -> Result<Vec<Entry>, VecDbError> {
        if query_vector.len() != self.vector_dim {
            return Ok(Vec::new());
        }

        let results = self
            .table
            .query()
            .nearest_to(query_vector)?
            .limit(limit)
            .execute()
            .await?;

        let batches: Vec<RecordBatch> = results.try_collect().await?;
        let mut entries = Vec::new();

        for batch in batches {
            let ids = batch
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| VecDbError::Database("missing id column".into()))?;
            let contents = batch
                .column(1)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| VecDbError::Database("missing content column".into()))?;

            for i in 0..batch.num_rows() {
                entries.push(Entry {
                    id: ids.value(i).to_string(),
                    content: contents.value(i).to_string(),
                });
            }
        }

        Ok(entries)
    }

    #[allow(dead_code)]
    pub async fn exists_by_content(&self, content: &str) -> Result<bool, VecDbError> {
        Ok(self.find_id_by_content(content).await?.is_some())
    }

    #[allow(dead_code)]
    pub async fn patch(
        &self,
        id: &str,
        new_content: &str,
        new_vector: Vec<f32>,
    ) -> Result<(), VecDbError> {
        if new_vector.len() != self.vector_dim {
            return Err(VecDbError::DimensionMismatch {
                expected: self.vector_dim,
                actual: new_vector.len(),
            });
        }

        self.delete(id).await?;
        self.post(id, new_content, new_vector).await?;
        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<(), VecDbError> {
        validate_id(id)?;

        let predicate = format!("id = '{id}'");
        let count = self.table.count_rows(Some(predicate.clone())).await?;

        if count == 0 {
            return Err(VecDbError::NotFound(id.to_string()));
        }

        self.table.delete(predicate.as_str()).await?;
        Ok(())
    }

    pub async fn entry_count(&self) -> Result<usize, VecDbError> {
        Ok(self.table.count_rows(None::<String>).await?)
    }

    async fn find_id_by_content(&self, content: &str) -> Result<Option<String>, VecDbError> {
        let escaped = content.replace('\'', "''");
        let results = self
            .table
            .query()
            .only_if(format!("content = '{escaped}'"))
            .limit(1)
            .execute()
            .await?;

        let batches: Vec<RecordBatch> = results.try_collect().await?;
        for batch in batches {
            let ids = batch
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| VecDbError::Database("missing id column".into()))?;
            if ids.len() > 0 {
                return Ok(Some(ids.value(0).to_string()));
            }
        }
        Ok(None)
    }
}
