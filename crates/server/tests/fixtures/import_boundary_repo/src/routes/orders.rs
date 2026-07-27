use crate::repositories::orders_repo::OrdersRepo;

pub fn list_orders_handler(repo: &OrdersRepo) -> usize {
    repo.count()
}
