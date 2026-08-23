use crate::domain::order::Order;

pub struct OrdersRepo;

impl OrdersRepo {
    pub fn count(&self) -> usize {
        0
    }

    pub fn list(&self) -> Vec<Order> {
        Vec::new()
    }
}
