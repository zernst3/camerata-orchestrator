// FIXTURE (crates/server/tests/self_measurement_parity_e2e.rs).
//
// SHAPE 1 — A MULTI-SITE VIOLATION. The same rule (SEC-NO-RAW-SQL-CONCAT-1) is violated at
// THREE separate sites in this one file. The harness asserts row CONSERVATION over this: a
// merge may absorb two of these three into the third only if the absorption is observable in
// some artifact. Three separate merge data-loss bugs shipped silently because an over-merge is
// observationally identical to a detection miss (tracker C5), and a single-site fixture cannot
// tell the two apart at all.
//
// SHAPE 2 — A FINDING FAMILY SHARING ONE CONTAINER TOKEN. All three sites name the same table,
// `orders`. The negative merge veto (`objects_conflict`) requires fully disjoint object sets,
// so one shared container token disables it (tracker C4) and these three become merge
// candidates. That is exactly the condition the conservation invariant must survive.

pub fn find_order(order_id: &str) -> String {
    format!("SELECT * FROM orders WHERE id = {order_id}")
}

pub fn orders_for_customer(customer_id: &str) -> String {
    format!("SELECT * FROM orders WHERE customer_id = {customer_id} AND status = 'open'")
}

pub fn delete_order(order_id: &str) -> String {
    format!("DELETE FROM orders WHERE id = {order_id} AND archived = false")
}
