use super::*;
use crate::proof::{ProofFile, ProofVersion};
use tokio::io::AsyncReadExt;

fn session(root: &Path, id: &str) -> Result<Session, ApiError> {
    if id.is_empty() || id.contains(['/', '\\']) || id == "." || id == ".." {
        return Err(ApiError::not_found());
    }
    let session = Session::at(&session_dir(root, id));
    session.meta().map_err(|_| ApiError::not_found())?;
    Ok(session)
}

pub(super) async fn history(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Vec<ProofVersion>>, ApiError> {
    let session = session(&state.root, &id)?;
    let events = session.events().map_err(ApiError::server)?;
    Ok(Json(
        crate::proof::versions(&events).map_err(ApiError::server)?,
    ))
}

pub(super) async fn artifacts(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Vec<ProofVersion>>, ApiError> {
    let session = session(&state.root, &id)?;
    let events = session.events().map_err(ApiError::server)?;
    Ok(Json(
        crate::proof::artifact_history(&events).map_err(ApiError::server)?,
    ))
}

pub(super) async fn download(
    State(state): State<AppState>,
    AxumPath((id, file_id)): AxumPath<(String, String)>,
) -> Result<Response, ApiError> {
    let session = session(&state.root, &id)?;
    let events = session.events().map_err(ApiError::server)?;
    let file: ProofFile = crate::proof::downloadable_files(&events)
        .map_err(ApiError::server)?
        .into_iter()
        .find(|file| file.id == file_id)
        .ok_or_else(ApiError::not_found)?;
    let input = crate::proof::open_file(&session, &file_id).map_err(|_| ApiError::not_found())?;
    let stream = futures_util::stream::try_unfold(
        tokio::fs::File::from_std(input),
        |mut input| async move {
            let mut bytes = vec![0; 65536];
            let read = input.read(&mut bytes).await?;
            if read == 0 {
                return Ok::<_, std::io::Error>(None);
            }
            bytes.truncate(read);
            Ok(Some((bytes, input)))
        },
    );
    let encoded_name: String = file
        .name
        .bytes()
        .map(|byte| format!("%{byte:02X}"))
        .collect();
    Response::builder()
        .header(header::CONTENT_TYPE, file.media_type)
        .header(header::CONTENT_LENGTH, file.size)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename*=UTF-8''{encoded_name}"),
        )
        .header("x-content-type-options", "nosniff")
        .header("content-security-policy", "sandbox; default-src 'none'")
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(axum::body::Body::from_stream(stream))
        .map_err(ApiError::server)
}
