use crate::error::{AppError, AppResult};
use axum::extract::{Multipart, Path, State};
use axum::Json;
use deadpool_sqlite::Pool;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use rusqlite::params;
use serde::Serialize;
use std::io::{Read, Write};
use std::sync::Arc;

pub struct SourceMapState {
    pub pool: Pool,
}

#[derive(Debug, Serialize)]
pub struct SourceMapEntry {
    pub id: i64,
    pub project_id: String,
    pub release: String,
    pub filename: String,
    pub uploaded_at: i64,
    pub size_bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct DeobfuscatedFrame {
    pub original_file: Option<String>,
    pub original_line: Option<u32>,
    pub original_column: Option<u32>,
    pub original_name: Option<String>,
    pub raw_frame: String,
}

/// POST /v1/projects/:slug/sourcemaps - Upload a source map file (multipart).
pub async fn upload_sourcemap(
    State(state): State<Arc<SourceMapState>>,
    Path(slug): Path<String>,
    mut multipart: Multipart,
) -> AppResult<Json<SourceMapEntry>> {
    let mut release: Option<String> = None;
    let mut filename: Option<String> = None;
    let mut map_data: Option<Vec<u8>> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::Validation(format!("multipart error: {e}")))?
    {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "release" => {
                let text: String = field
                    .text()
                    .await
                    .map_err(|e| AppError::Validation(format!("invalid release field: {e}")))?;
                release = Some(text);
            }
            "file" => {
                filename = field.file_name().map(|s: &str| s.to_string());
                let bytes: axum::body::Bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::Validation(format!("file read error: {e}")))?;
                map_data = Some(bytes.to_vec());
            }
            _ => {}
        }
    }

    let release = release.ok_or_else(|| AppError::Validation("missing 'release' field".into()))?;
    let filename =
        filename.ok_or_else(|| AppError::Validation("missing file with filename".into()))?;
    let raw_data = map_data.ok_or_else(|| AppError::Validation("missing 'file' field".into()))?;

    // Validate it's parseable as a source map
    sourcemap::SourceMap::from_reader(raw_data.as_slice())
        .map_err(|e| AppError::Validation(format!("invalid source map: {e}")))?;

    // Gzip compress
    let compressed = gzip_compress(&raw_data)
        .map_err(|e| AppError::Internal(format!("compression error: {e}")))?;
    let size_bytes = raw_data.len();

    let project_slug = slug.clone();
    let rel = release.clone();
    let fname = filename.clone();
    let now = chrono::Utc::now().timestamp();

    let conn = state
        .pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let id = conn
        .interact(move |conn| {
            // Look up project_id from slug
            let project_id: String = conn.query_row(
                "SELECT id FROM projects WHERE slug = ?1",
                params![project_slug],
                |row| row.get(0),
            )?;

            conn.execute(
                "INSERT INTO source_maps (project_id, release, filename, map_data, uploaded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (project_id, release, filename) DO UPDATE SET
                    map_data = excluded.map_data,
                    uploaded_at = excluded.uploaded_at",
                params![project_id, rel, fname, compressed, now],
            )?;

            Ok::<_, rusqlite::Error>(conn.last_insert_rowid())
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(Json(SourceMapEntry {
        id,
        project_id: slug,
        release,
        filename,
        uploaded_at: now,
        size_bytes,
    }))
}

/// GET /v1/projects/:slug/sourcemaps - List uploaded source maps.
pub async fn list_sourcemaps(
    State(state): State<Arc<SourceMapState>>,
    Path(slug): Path<String>,
) -> AppResult<Json<Vec<SourceMapEntry>>> {
    let conn = state
        .pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let entries = conn
        .interact(move |conn| {
            let project_id: String = conn.query_row(
                "SELECT id FROM projects WHERE slug = ?1",
                params![slug],
                |row| row.get(0),
            )?;

            let mut stmt = conn.prepare(
                "SELECT id, project_id, release, filename, uploaded_at, LENGTH(map_data)
                 FROM source_maps WHERE project_id = ?1
                 ORDER BY uploaded_at DESC",
            )?;
            let rows = stmt
                .query_map(params![project_id], |row| {
                    Ok(SourceMapEntry {
                        id: row.get(0)?,
                        project_id: row.get(1)?,
                        release: row.get(2)?,
                        filename: row.get(3)?,
                        uploaded_at: row.get(4)?,
                        size_bytes: row.get::<_, i64>(5)? as usize,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, rusqlite::Error>(rows)
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    Ok(Json(entries))
}

/// DELETE /v1/projects/:slug/sourcemaps/:id - Delete a source map.
pub async fn delete_sourcemap(
    State(state): State<Arc<SourceMapState>>,
    Path((slug, map_id)): Path<(String, i64)>,
) -> AppResult<Json<serde_json::Value>> {
    let conn = state
        .pool
        .get()
        .await
        .map_err(|e| AppError::Internal(format!("pool error: {e}")))?;

    let deleted = conn
        .interact(move |conn| {
            let project_id: String = conn.query_row(
                "SELECT id FROM projects WHERE slug = ?1",
                params![slug],
                |row| row.get(0),
            )?;
            conn.execute(
                "DELETE FROM source_maps WHERE id = ?1 AND project_id = ?2",
                params![map_id, project_id],
            )
        })
        .await
        .map_err(|e| AppError::Internal(format!("interact error: {e}")))??;

    if deleted == 0 {
        return Err(AppError::NotFound(format!("source map {map_id} not found")));
    }

    Ok(Json(serde_json::json!({ "deleted": map_id })))
}

/// Deobfuscate a stack trace using source maps for a given project+release.
pub fn deobfuscate_stack(
    conn: &rusqlite::Connection,
    project_id: &str,
    release: &str,
    stack: &str,
) -> Vec<DeobfuscatedFrame> {
    // Load all source maps for this project+release
    let maps = load_source_maps(conn, project_id, release);
    if maps.is_empty() {
        return vec![];
    }

    let mut frames = Vec::new();
    for line in stack.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Try to parse frame: "at functionName (file.js:line:col)" or "file.js:line:col"
        if let Some(frame) = parse_frame(trimmed) {
            let deobfuscated = lookup_frame(&maps, &frame);
            frames.push(deobfuscated);
        } else {
            frames.push(DeobfuscatedFrame {
                original_file: None,
                original_line: None,
                original_column: None,
                original_name: None,
                raw_frame: trimmed.to_string(),
            });
        }
    }

    frames
}

struct ParsedFrame {
    file: String,
    line: u32,
    column: u32,
    raw: String,
}

fn parse_frame(line: &str) -> Option<ParsedFrame> {
    // Match patterns like:
    // "at func (file.js:10:5)"
    // "file.js:10:5"
    // "at file.js:10:5"
    let re_paren = regex::Regex::new(r"\(([^)]+):(\d+):(\d+)\)").ok()?;
    let re_bare = regex::Regex::new(r"(?:at\s+)?(\S+):(\d+):(\d+)").ok()?;

    if let Some(caps) = re_paren.captures(line) {
        return Some(ParsedFrame {
            file: caps[1].to_string(),
            line: caps[2].parse().ok()?,
            column: caps[3].parse().ok()?,
            raw: line.to_string(),
        });
    }

    if let Some(caps) = re_bare.captures(line) {
        return Some(ParsedFrame {
            file: caps[1].to_string(),
            line: caps[2].parse().ok()?,
            column: caps[3].parse().ok()?,
            raw: line.to_string(),
        });
    }

    None
}

fn lookup_frame(maps: &[(String, sourcemap::SourceMap)], frame: &ParsedFrame) -> DeobfuscatedFrame {
    // Find matching source map by filename
    for (filename, sm) in maps {
        // Match if the frame file ends with the map filename (sans .map)
        let base = filename.trim_end_matches(".map");
        if frame.file.ends_with(base) || frame.file == *filename {
            // Source map lines/cols are 0-indexed
            let line = if frame.line > 0 { frame.line - 1 } else { 0 };
            let col = if frame.column > 0 {
                frame.column - 1
            } else {
                0
            };

            if let Some(token) = sm.lookup_token(line, col) {
                return DeobfuscatedFrame {
                    original_file: token.get_source().map(|s: &str| s.to_string()),
                    original_line: Some(token.get_src_line() + 1),
                    original_column: Some(token.get_src_col()),
                    original_name: token.get_name().map(|s: &str| s.to_string()),
                    raw_frame: frame.raw.clone(),
                };
            }
        }
    }

    DeobfuscatedFrame {
        original_file: None,
        original_line: None,
        original_column: None,
        original_name: None,
        raw_frame: frame.raw.clone(),
    }
}

fn load_source_maps(
    conn: &rusqlite::Connection,
    project_id: &str,
    release: &str,
) -> Vec<(String, sourcemap::SourceMap)> {
    let mut stmt = match conn.prepare(
        "SELECT filename, map_data FROM source_maps WHERE project_id = ?1 AND release = ?2",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let rows = match stmt.query_map(params![project_id, release], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
    }) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let mut maps = Vec::new();
    for (filename, compressed_data) in rows.flatten() {
        // Decompress gzip
        if let Ok(raw) = gzip_decompress(&compressed_data) {
            if let Ok(sm) = sourcemap::SourceMap::from_reader(raw.as_slice()) {
                maps.push((filename, sm));
            }
        }
    }

    maps
}

fn gzip_compress(data: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    encoder.finish()
}

fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    const MAX_DECOMPRESSED_SIZE: usize = 32 * 1024 * 1024; // 32MB limit
    let mut decoder = GzDecoder::new(data);
    let mut result = Vec::new();
    decoder
        .by_ref()
        .take(MAX_DECOMPRESSED_SIZE as u64 + 1)
        .read_to_end(&mut result)?;
    if result.len() > MAX_DECOMPRESSED_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "decompressed source map exceeds 32MB limit",
        ));
    }
    Ok(result)
}
