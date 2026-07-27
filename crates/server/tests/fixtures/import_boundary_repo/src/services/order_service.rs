use crate::repositories::orders_repo::OrdersRepo;
use crate::domain::order::Order;

pub struct OrderService<'a> {
    repo: &'a OrdersRepo,
}

impl<'a> OrderService<'a> {
    pub fn list_orders(&self) -> Vec<Order> {
        Vec::new()
    }
}
