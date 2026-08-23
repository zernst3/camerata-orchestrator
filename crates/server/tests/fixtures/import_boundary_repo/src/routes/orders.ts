import { OrdersRepo } from '../repositories/orders_repo';

export function listOrdersHandler(repo: OrdersRepo) {
  return repo.count();
}
