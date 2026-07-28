import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// El 5174 evita chocar con TapoController, que usa el 5173.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    // Explicito a proposito: por defecto Vite puede acabar escuchando solo en
    // ::1, y entonces el devUrl de Tauri (127.0.0.1) recibe un connection
    // refused y la ventana muestra una pagina de error del navegador.
    host: "127.0.0.1",
    port: 5174,
    strictPort: true,
  },
  build: {
    target: "esnext",
  },
});
