import { defineConfig } from "vite";

export default defineConfig({
  resolve: {
    alias: {
      react: "preact/compat",
      "react-dom": "preact/compat",
      "react-dom/client": "preact/compat",
      "react/jsx-runtime": "preact/jsx-runtime",
      "react/jsx-dev-runtime": "preact/jsx-dev-runtime",
    },
  },
  server: {
    host: true,
    cors: true,
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // Increase chunk size warning limit
    chunkSizeWarningLimit: 1000,
    rollupOptions: {
      onwarn(warning, warn) {
        // Suppress eval warnings from tseep
        if (warning.code === 'EVAL' && warning.id?.includes('tseep')) {
          return;
        }
        warn(warning);
      },
      output: {
        manualChunks(id) {
          // NDK and related packages
          if (id.includes('@nostr-dev-kit/ndk') || 
              id.includes('nostr-tools') || 
              id.includes('tseep')) {
            return 'ndk';
          }
          // Vendor packages
          if (id.includes('node_modules') && 
              (id.includes('preact') || 
               id.includes('localforage') || 
               id.includes('@cashu/cashu-ts'))) {
            return 'vendor';
          }
        }
      }
    }
  },
});
