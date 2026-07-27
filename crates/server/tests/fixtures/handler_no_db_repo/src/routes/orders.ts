export function listOrders(db: Db) {
  return db.query('select * from orders');
}
