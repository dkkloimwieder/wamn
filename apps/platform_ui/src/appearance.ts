/* WFE v0.0.2 DefaultAppearance: Light, Blue, depth 2; canvas 1, pane 0, content 0.
   Consumer contract: styles/README.md, Library defaults and application overrides.
Open Props samples. MIT, Copyright (c) 2021 Adam Argyle.
License: MIT license below
Source: https://raw.githubusercontent.com/argyleink/open-props/f39ab7c377a0e047f930982e3611bbc95254a260/src/props.colors.js
Source SHA-256: 9197aaccf67264fa1f4acc6c19504f3b79fa7c4c1d684af7e29368dcca4cf39f
Accent and table-divider samples retain the Open Props provenance above.
Local adaptation: neutral roles match the measured reference presentation.
Surface 1 is the section; Surface 2 provides slight canvas contrast.

MIT License

Copyright (c) 2021 Adam Argyle

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/
// Shadow source: https://raw.githubusercontent.com/argyleink/open-props/f39ab7c377a0e047f930982e3611bbc95254a260/src/props.shadows.js
// SHA-256: b5a0f12701b3de2c9d142f6d5fb224d621cd9ffeeff3fdac3d631a2d6bfe0e2f
import type { JSX } from "@solidjs/web";

export const appearance: JSX.CSSProperties = {
  "--ui-font-size": "12px",
  "--ui-small-font-size": "12px",
  "--ui-font-family": '"Fira Code Variable", ui-monospace, monospace',
  "--ui-line-height": "1.625",
  "--ui-control-line-height": "16px",
  "--ui-label-line-height": "12px",
  "--ui-label-weight": "400",
  "--ui-heading-font-size": "14px",
  "--ui-heading-line-height": "20px",
  "--ui-heading-weight": "500",
  "--ui-button-font-weight": "500",
  "--ui-control-height": "32px",
  "--ui-control-padding-y": "4px",
  "--ui-control-padding-x": "10px",
  "--ui-input-padding-x": "10px",
  "--ui-gap": "8px",
  "--ui-layout-inset": "16px",
  "--ui-field-gap": "20px",
  "--ui-field-max-width": "30rem",
  "--ui-form-max-width": "30rem",
  "--ui-table-control-height": "26px",
  "--ui-secondary-accent": "#1971c2",
  "--ui-sidebar-accent": "#1971c2",
  "--ui-sidebar-neutral": "#262626",
  "--ui-decoration-border": "color-mix(in srgb, #737373 20%, transparent)",
  "--ui-decoration-dot-opacity": "35%",
  "--ui-decoration-border-opacity": "20%",
  "--ui-action-dot-visibility": "100%",
  "--ui-color-scheme": "light",
  "--ui-canvas-surface": "#fcfcfd",
  "--ui-pane-surface": "#ffffff",
  "--ui-surface": "#ffffff",
  "--ui-muted-surface": "#ffffff",
  "--ui-skeleton-surface": "#f5f5f5",
  "--ui-popup-surface": "#ffffff",
  "--ui-highlight": "#f5f5f5",
  "--ui-hover": "#f5f5f5",
  "--ui-text": "#262626",
  "--ui-muted-text": "#737373",
  "--ui-subtle-text": "#737373",
  "--ui-border": "#e5e5e5",
  "--ui-divider": "#e5e5e5",
  "--ui-table-row-divider": "#e9ecef",
  "--ui-table-column-divider": "#e9ecef",
  "--ui-control-border": "#e5e5e5",
  "--ui-control-surface": "#ffffff",
  "--ui-control-hover": "#ffffff",
  "--ui-focus-width": "1px",
  "--ui-focus-offset": "0px",
  "--ui-field-focus-border": "#a3a3a3",
  "--ui-button-border": "#e5e5e5",
  "--ui-canvas-pattern":
    "radial-gradient(circle, color-mix(in srgb, #737373 35%, transparent) 0.5px, transparent 0.75px)",
  "--ui-canvas-pattern-size": "0.3em 0.3em",
  "--ui-handle-border": "#e5e5e5",
  "--ui-focus": "#1971c2",
  "--ui-focus-ring": "color-mix(in srgb, #a3a3a3 50%, transparent)",
  "--ui-selection": "#1971c2",
  "--ui-on-selection": "#ffffff",
  "--ui-accent": "#1971c2",
  "--ui-on-accent": "#ffffff",
  "--ui-accent-hover": "color-mix(in srgb, #1971c2 90%, #000000)",
  "--ui-accent-inverted-hover": "color-mix(in srgb, #ffffff 95%, #1971c2)",
  "--ui-shadow-overflow": "24px",
  "--ui-panel-shadow": "var(--review-shadow-2)",
  "--ui-popup-shadow": "var(--review-shadow-3)",
  "--review-shadow-color": "220 3% 15%",
  "--review-shadow-strength": "1%",
  "--ui-button-color": "var(--ui-accent)",
  "--ui-button-on-color": "var(--ui-on-accent)",
  "--review-shadow-strength-3": "calc(var(--review-shadow-strength) + 2%)",
  "--review-shadow-strength-4": "calc(var(--review-shadow-strength) + 3%)",
  "--review-shadow-strength-5": "calc(var(--review-shadow-strength) + 4%)",
  "--review-shadow-strength-6": "calc(var(--review-shadow-strength) + 5%)",
  "--review-shadow-strength-8": "calc(var(--review-shadow-strength) + 7%)",
  "--review-shadow-strength-10": "calc(var(--review-shadow-strength) + 9%)",
  "--review-shadow-1": "0 1px 2px -1px hsl(var(--review-shadow-color) / var(--review-shadow-strength-10))",
  "--review-shadow-2":
    "0 3px 5px -2px hsl(var(--review-shadow-color) / var(--review-shadow-strength-4)), 0 7px 14px -5px hsl(var(--review-shadow-color) / var(--review-shadow-strength-6))",
  "--review-shadow-3":
    "0 -1px 3px 0 hsl(var(--review-shadow-color) / var(--review-shadow-strength-3)), 0 1px 2px -5px hsl(var(--review-shadow-color) / var(--review-shadow-strength-3)), 0 2px 5px -5px hsl(var(--review-shadow-color) / var(--review-shadow-strength-5)), 0 4px 12px -5px hsl(var(--review-shadow-color) / var(--review-shadow-strength-6)), 0 12px 15px -5px hsl(var(--review-shadow-color) / var(--review-shadow-strength-8))",
};
