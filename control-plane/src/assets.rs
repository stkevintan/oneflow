use axum::{
    body::Body,
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

/// Static frontend assets, embedded into the binary at compile time.
///
/// The `dist/` directory is produced by `npm run build` in `web/`. In debug builds
/// `rust-embed` serves the files from disk so frontend edits show up without
/// recompiling; release builds embed the bytes directly.
#[derive(RustEmbed)]
#[folder = "../web/dist"]
struct Assets;

/// Serves the embedded frontend and falls back to `index.html` for unknown paths.
pub async fn static_handler(uri: Uri) -> Response {
    let requested = uri.path().trim_start_matches('/');
    let path = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };

    if let Some(file) = Assets::get(path) {
        return serve(path, file.data.into_owned());
    }

    match Assets::get("index.html") {
        Some(file) => serve("index.html", file.data.into_owned()),
        // A fresh checkout has no built bundle; explain what to run instead of
        // returning a bare 404.
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(NOT_BUILT_PAGE))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    }
}

const NOT_BUILT_PAGE: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>LLM Control Plane &mdash; dashboard not built</title>
    <style>
      body { margin: 0; min-height: 100vh; display: grid; place-items: center;
             background: #020617; color: #e2e8f0;
             font-family: ui-sans-serif, system-ui, sans-serif; }
      main { max-width: 44rem; padding: 2rem; border: 1px solid #1e293b;
             border-radius: .75rem; background: #0f172a; }
      h1 { margin: 0 0 .5rem; font-size: 1.15rem; }
      p { margin: .5rem 0; color: #94a3b8; line-height: 1.6; }
      pre { margin: 1rem 0 0; padding: .85rem 1rem; border: 1px solid #1e293b;
            border-radius: .5rem; background: #020617; color: #e2e8f0;
            font-family: ui-monospace, Menlo, monospace; font-size: .85rem; }
    </style>
  </head>
  <body>
    <main>
      <h1>The React dashboard has not been built</h1>
      <p>The control-plane JSON API is available; only the dashboard bundle is missing.</p>
      <pre>cd web &amp;&amp; npm install &amp;&amp; npm run build</pre>
      <p>Then rebuild the control plane and reload. See <code>web/README.md</code>.</p>
    </main>
  </body>
</html>
"#;

fn serve(path: &str, bytes: Vec<u8>) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    Response::builder()
        .header(header::CONTENT_TYPE, mime.as_ref())
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
