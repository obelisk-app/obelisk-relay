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
          if (!id.includes('node_modules')) return;

          // Keep the Preact runtime isolated. When it shares a chunk with
          // packages that import nostr-tools, Rollup can create a circular
          // vendor <-> ndk chunk dependency that runs JSX before Preact's
          // options object is initialized.
          if (
            id.includes('/preact/') ||
            id.includes('/preact-router/') ||
            id.includes('/@preact/')
          ) {
            return 'preact';
          }

          if (
            id.includes('/@nostr-wot/ui/') ||
            id.includes('/@nostr-wot/data/') ||
            id.includes('/@nostr-wot/signers/')
          ) {
            return 'nostr-wot';
          }

          if (
            id.includes('/@nostr-dev-kit/ndk/') ||
            id.includes('/nostr-tools/') ||
            id.includes('/tseep/')
          ) {
            return 'ndk';
          }

          if (id.includes('/@cashu/cashu-ts/')) {
            return 'cashu';
          }

          if (id.includes('/localforage/')) {
            return 'vendor';
          }
        }
      }
    }
  },
});
