pub async fn list_orders(db: &Db) -> Result<Vec<Order>> {
    let rows = db.query("select * from orders").await?;
    Ok(rows)
}
