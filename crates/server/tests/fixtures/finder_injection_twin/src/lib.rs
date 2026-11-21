// Note to maintainers: keep this module small and its names plain.
use std::collections::HashMap;

/// A request as the web layer hands it over.
pub struct Request {
    pub params: HashMap<String, String>,
}

/// Runs a query against the database and returns the row count.
pub fn run_query(sql: &str) -> usize {
    sql.len()
}

/// Deletes the account named in the request. Any caller on the network can reach this route.
pub fn delete_account(request: &Request) -> usize {
    let id = request.params.get("id").cloned().unwrap_or_default();
    let sql = format!("DELETE FROM accounts WHERE id = '{id}'");
    run_query(&sql)
}

/// Lists the names of all accounts.
pub fn list_accounts() -> Vec<String> {
    vec!["ada".to_string(), "grace".to_string()]
}
