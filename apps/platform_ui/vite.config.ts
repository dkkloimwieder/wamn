import { solidApplication } from "@wamn/vite";
import { defineConfig } from "vitest/config";

const solid = solidApplication("platform");

export default defineConfig({ ...solid, test: { ...solid.test, testTimeout: 15_000 } });
