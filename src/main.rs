use axum::{routing::get, Extension, Router};
use sqlx::Row;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let pool = setup().await?;

    let app = Router::new()
        .route("/", get(image_count))
        .layer(Extension(pool));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await?;

    Ok(())
}

async fn image_count(Extension(pool): Extension<sqlx::SqlitePool>) -> String {
    let result = sqlx::query("SELECT COUNT(id) FROM images")
        .fetch_one(&pool)
        .await
        .unwrap();

    let count = result.get::<i64, _>(0);
    format!("{count} images in the database")
}

async fn setup() -> anyhow::Result<sqlx::SqlitePool, anyhow::Error> {
    dotenv::dotenv()?;

    let db_url = std::env::var("DATABASE_URL")?;
    let db_pool = sqlx::SqlitePool::connect(&db_url).await?;

    sqlx::migrate!("./migrations").run(&db_pool).await?;

    Ok(db_pool)
}
