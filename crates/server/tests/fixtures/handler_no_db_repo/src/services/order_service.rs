pub async fn place_order(db: &Db) -> Result<()> {
    db.transaction(|tx| async move { tx.insert("orders").await }).await
}
