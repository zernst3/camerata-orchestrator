import { app } from "./firebase";

export function App() {
  return <p>{app.name}</p>;
}
