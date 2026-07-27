pub async fn fetch_all(db: &Db) -> Result<Vec<Order>> {
    db.query("select * from orders").await
}
