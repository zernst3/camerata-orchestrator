import { onRequest } from "firebase-functions/v2/https";
import { getFirestore } from "firebase-admin/firestore";

const db = getFirestore();

export const createOrder = onRequest(async (req, res) => {
  await db.collection("orders").add(req.body);
  res.send("ok");
});
