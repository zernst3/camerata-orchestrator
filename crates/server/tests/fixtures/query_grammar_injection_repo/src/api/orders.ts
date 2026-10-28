// Orders search — plants the D3 problem statement: a PostgREST `.or()` filter built from
// request-derived input via template-literal interpolation, next to two safe siblings.

export async function searchOrders(supabase, req) {
  const { data } = await supabase
    .from('orders')
    .or(`user_id.eq.${req.query.userId},status.eq.${req.query.status}`);
  return data;
}

export async function searchOrdersSafe(supabase, status) {
  const { data } = await supabase.from('orders').eq('status', status);
  return data;
}

export async function searchOrdersStatic(supabase) {
  const { data } = await supabase
    .from('orders')
    .or('status.eq.active,status.eq.pending');
  return data;
}
