//! One error type for every handler, so failures come back as JSON rather than
//! a bare status line (see the `Json<T>` convention in CLAUDE.md).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

pub type ApiResult<T> = Result<Json<T>, ApiError>;

pub struct ApiError {
    status: StatusCode,
    message: String,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl ApiError {
    pub fn not_found(what: impl std::fmt::Display) -> Self {
        ApiError {
            status: StatusCode::NOT_FOUND,
            message: what.to_string(),
        }
    }

    /// A well-formed request that cannot be carried out right now — e.g.
    /// play-all when every TV is offline.
    pub fn conflict(why: impl std::fmt::Display) -> Self {
        ApiError {
            status: StatusCode::CONFLICT,
            message: why.to_string(),
        }
    }

    pub fn bad_request(why: impl std::fmt::Display) -> Self {
        ApiError {
            status: StatusCode::BAD_REQUEST,
            message: why.to_string(),
        }
    }
}

/// Anything that fails with `anyhow` becomes a 500.
///
/// The full cause chain goes to the log; the response carries only the
/// top-level message. This is a LAN tool with one operator, so the message is
/// worth returning — but the chain can name tables and SQL, which belongs in
/// the log rather than on the wire.
impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        tracing::error!(error = format!("{err:#}"), "request failed");
        ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: err.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}
