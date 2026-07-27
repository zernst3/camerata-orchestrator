async fn list_orgs_handler(db: &Db) -> Result<Vec<Org>> {
    let rows = db.query("select * from orgs").await?;
    Ok(rows)
}
