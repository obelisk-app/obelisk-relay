import js from '@eslint/js'
import tseslint from 'typescript-eslint'
import jsxA11y from 'eslint-plugin-jsx-a11y'
import reactHooks from 'eslint-plugin-react-hooks'
import globals from 'globals'

/**
 * The project had no linter at all, which is why a whole class of defect
 * accumulated unnoticed: 36 `<label>` elements with no `htmlFor`, hover-only
 * controls unreachable on touch, effects missing dependencies, and a form that
 * asked users to paste a private key. Most of that is mechanically detectable.
 *
 * Configured to report rather than block: `tsc` is already the build gate, and a
 * lint run that fails on the first of several hundred pre-existing warnings gets
 * switched off rather than fixed. `npm run lint` shows the backlog;
 * `npm run lint:fix` handles what is auto-fixable.
 */
export default tseslint.config(
  {
    // Build output, dependencies and generated assets.
    ignores: ['dist/**', 'node_modules/**', '*.config.js', '*.config.mts'],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ['**/*.{ts,tsx}'],
    languageOptions: {
      globals: { ...globals.browser },
      parserOptions: {
        ecmaFeatures: { jsx: true },
      },
    },
    plugins: {
      'jsx-a11y': jsxA11y,
      'react-hooks': reactHooks,
    },
    settings: {
      // Preact uses `class`, not `className`, and its components are the ones
      // jsx-a11y must inspect.
      'jsx-a11y': {
        polymorphicPropName: 'as',
        attributes: { for: ['htmlFor', 'for'] },
      },
    },
    rules: {
      ...jsxA11y.flatConfigs.recommended.rules,
      ...reactHooks.configs.recommended.rules,

      // The three that map directly to defects found in this codebase.
      'jsx-a11y/label-has-associated-control': [
        'warn',
        { assert: 'either', depth: 3 },
      ],
      'jsx-a11y/no-noninteractive-element-interactions': 'warn',
      'jsx-a11y/click-events-have-key-events': 'warn',

      // `any` is pervasive in the legacy chat app and in the NDK boundary
      // types. Worth seeing, not worth blocking on.
      '@typescript-eslint/no-explicit-any': 'warn',
      '@typescript-eslint/no-unused-vars': [
        'warn',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_' },
      ],
      // An empty catch is what hid every wallet failure in this app; the fix is
      // to log, so flag the shape rather than allowing it.
      'no-empty': ['warn', { allowEmptyCatch: false }],
    },
  },
)
