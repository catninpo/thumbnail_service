use axum::{
    extract::{Multipart, Path},
    http::{header, HeaderMap},
    response::{Html, IntoResponse},
    routing::{get, post},
    Extension, Form, Json, Router,
};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Pool, Row, Sqlite};
use tokio_util::io::ReaderStream;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let pool = setup().await?;

    let app = Router::new()
        .route("/", get(home_page))
        .route("/upload", post(uploader))
        .route("/image/:id", get(get_image))
        .route("/thumb/:id", get(get_thumbnail))
        .route("/images", get(list_images))
        .route("/images-html", get(render_images))
        .route("/image-count", get(image_count_page))
        .route("/search", post(search_images))
        .layer(Extension(pool));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await?;

    Ok(())
}

async fn setup() -> anyhow::Result<sqlx::SqlitePool, anyhow::Error> {
    dotenv::dotenv()?;

    let db_url = std::env::var("DATABASE_URL")?;
    let db_pool = sqlx::SqlitePool::connect(&db_url).await?;

    sqlx::migrate!("./migrations").run(&db_pool).await?;

    fill_missing_thumbnails(&db_pool).await?;

    Ok(db_pool)
}

async fn image_count_page(Extension(pool): Extension<sqlx::SqlitePool>) -> String {
    let result = sqlx::query("SELECT COUNT(id) FROM images")
        .fetch_one(&pool)
        .await
        .unwrap();

    let count = result.get::<i64, _>(0);
    format!("{count} images in the database")
}

async fn home_page() -> Html<String> {
    let path = std::path::Path::new("src/pages/index.html");
    let content = tokio::fs::read_to_string(path).await.unwrap();

    Html(content)
}

async fn store_image_to_database(pool: &sqlx::SqlitePool, tags: &str) -> anyhow::Result<i64> {
    let row = sqlx::query("INSERT INTO images (tags) VALUES (?) RETURNING id")
        .bind(tags)
        .fetch_one(pool)
        .await?;

    Ok(row.get(0))
}

async fn save_image(id: i64, bytes: &[u8]) -> anyhow::Result<()> {
    let base_path = std::path::Path::new("images");
    if !base_path.exists() || !base_path.is_dir() {
        tokio::fs::create_dir_all(base_path).await?;
    }

    let image_path = base_path.join(format!("{id}.jpg"));
    if image_path.exists() {
        anyhow::bail!("File already exists");
    }

    tokio::fs::write(image_path, bytes).await?;

    Ok(())
}

async fn get_image(Path(id): Path<i64>) -> impl IntoResponse {
    let filename = format!("images/{id}.jpg");
    let attachment = format!("filename={filename}");
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("image/jpeg"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_str(&attachment).unwrap(),
    );

    let file = tokio::fs::File::open(&filename).await.unwrap();

    axum::response::Response::builder()
        .header(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("image/jpeg"),
        )
        .header(
            header::CONTENT_DISPOSITION,
            header::HeaderValue::from_str(&attachment).unwrap(),
        )
        .body(axum::body::Body::from_stream(ReaderStream::new(file)))
        .unwrap()
}

// TODO: Make generic with get_image
async fn get_thumbnail(Path(id): Path<i64>) -> impl IntoResponse {
    let filename = format!("images/{id}_thumb.jpg");
    let attachment = format!("filename={filename}");
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("image/jpeg"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_str(&attachment).unwrap(),
    );

    let file = tokio::fs::File::open(&filename).await.unwrap();

    axum::response::Response::builder()
        .header(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("image/jpeg"),
        )
        .header(
            header::CONTENT_DISPOSITION,
            header::HeaderValue::from_str(&attachment).unwrap(),
        )
        .body(axum::body::Body::from_stream(ReaderStream::new(file)))
        .unwrap()
}

async fn uploader(
    Extension(pool): Extension<sqlx::SqlitePool>,
    mut multipart: Multipart,
) -> Html<String> {
    let mut tags = None;
    let mut image = None;

    while let Some(field) = multipart.next_field().await.unwrap() {
        let name = field.name().unwrap().to_string();
        let data = field.bytes().await.unwrap();

        match name.as_str() {
            "tags" => tags = Some(String::from_utf8(data.to_vec()).unwrap()),
            "image" => image = Some(data.to_vec()),
            _ => panic!("Unknown field: {name}"), // TODO: Handle Error.
        }
    }

    let p = std::path::Path::new("src/pages/thumbnail.html");
    let mut template = tokio::fs::read_to_string(p).await.unwrap();

    if let (Some(tags), Some(image)) = (tags, image) {
        // TODO: Return response header instead on failure rather than erroring out.
        let image_id = store_image_to_database(&pool, &tags).await.unwrap();
        save_image(image_id, &image).await.unwrap();
        make_thumbnail(image_id).await.unwrap();

        template = template.replace("{tags}", &tags);
        template = template.replace("{id}", &image_id.to_string());
    } else {
        panic!("Missing field"); // TODO: Handle Error. -> Return 400 Bad Request
    }

    Html(template.to_string())
}

async fn fill_missing_thumbnails(pool: &Pool<Sqlite>) -> anyhow::Result<()> {
    let mut rows = sqlx::query("SELECT id FROM images").fetch(pool);

    while let Some(row) = rows.try_next().await? {
        let id = row.get::<i64, _>(0);
        let thumbnail_path = format!("images/{id}_thumb.jpg");
        if !std::path::Path::new(&thumbnail_path).exists() {
            make_thumbnail(id).await?;
        }
    }

    Ok(())
}

async fn make_thumbnail(id: i64) -> anyhow::Result<()> {
    let image_path = format!("images/{id}.jpg");
    let thumbnail_path = format!("images/{id}_thumb.jpg");
    let image_bytes: Vec<u8> = std::fs::read(image_path)?;

    let image = if let Ok(format) = image::guess_format(&image_bytes) {
        image::load_from_memory_with_format(&image_bytes, format)?
    } else {
        image::load_from_memory(&image_bytes)?
    };

    let thumbnail = image.thumbnail(100, 100);
    thumbnail.save(thumbnail_path)?;

    Ok(())
}

#[derive(Deserialize, Serialize, FromRow, Debug)]
struct ImageRecord {
    id: i64,
    tags: String,
}

async fn list_images(Extension(pool): Extension<sqlx::SqlitePool>) -> Json<Vec<ImageRecord>> {
    sqlx::query_as::<_, ImageRecord>("SELECT id, tags FROM images ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap()
        .into()
}

async fn render_images(Extension(pool): Extension<sqlx::SqlitePool>) -> Html<String> {
    let images = sqlx::query_as::<_, ImageRecord>("SELECT id, tags FROM images ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();

    let p = std::path::Path::new("src/pages/thumbnail.html");
    let template = tokio::fs::read_to_string(p).await.unwrap();

    let mut image_html = String::new();
    for image in images {
        let mut _tmp = template.clone();
        _tmp = _tmp.replace("{tags}", &image.tags);
        _tmp = _tmp.replace("{id}", &image.id.to_string());

        image_html.push_str(&_tmp);
    }

    Html(image_html.to_string())
}

#[derive(Deserialize)]
struct Search {
    tags: String,
}

async fn search_images(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Form(form): Form<Search>,
) -> Html<String> {
    let tag = format!("%{}%", form.tags);

    let images = sqlx::query_as::<_, ImageRecord>(
        "SELECT id, tags FROM images WHERE tags LIKE ? ORDER BY id",
    )
    .bind(tag)
    .fetch_all(&pool)
    .await
    .unwrap();

    let p = std::path::Path::new("src/pages/thumbnail.html");
    let template = tokio::fs::read_to_string(p).await.unwrap();

    let mut image_html = String::new();
    for image in images {
        let mut _tmp = template.clone();
        _tmp = _tmp.replace("{tags}", &image.tags);
        _tmp = _tmp.replace("{id}", &image.id.to_string());

        image_html.push_str(&_tmp);
    }

    Html(image_html.to_string())
}
