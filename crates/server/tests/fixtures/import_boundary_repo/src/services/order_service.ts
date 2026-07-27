import { OrdersRepo } from '../repositories/orders_repo';
import { Order } from '../domain/order';

export class OrderService {
  constructor(private repo: OrdersRepo) {}

  listOrders(): Order[] {
    return [];
  }
}
