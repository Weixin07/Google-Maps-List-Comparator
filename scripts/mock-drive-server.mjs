#!/usr/bin/env node
import http from "node:http";
import { readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const qaDir = resolve(__dirname, "..", "qa");
const fixtures = {
  "list-a": readFileSync(resolve(qaDir, "list-a.kml"), "utf8"),
  "list-b": readFileSync(resolve(qaDir, "list-b.kml"), "utf8"),
};

const port = Number(process.env.MOCK_DRIVE_PORT ?? 8788);
const issuedDeviceCode = "mock-device-code";
const tokenResponse = {
  access_token: "ya29.mock.access",
  refresh_token: "ya29.mock.refresh",
  expires_in: 3600,
  scope: "drive.readonly openid email profile",
  token_type: "Bearer",
};

const server = http.createServer((req, res) => {
  if (req.method === "POST" && req.url === "/device/code") {
    respondJson(res, {
      device_code: issuedDeviceCode,
      user_code: "MOCK-CODE",
      verification_url: "https://example.com/activate",
      expires_in: 1800,
      interval: 5,
    });
    return;
  }

  if (req.method === "POST" && req.url === "/token") {
    respondJson(res, tokenResponse);
    return;
  }

  if (req.method === "GET" && req.url === "/userinfo") {
    respondJson(res, {
      email: "qa@example.com",
      name: "QA Importer",
      picture: null,
    });
    return;
  }

  if (req.method === "GET" && req.url?.startsWith("/drive/v3/files?")) {
    respondJson(res, {
      files: [
        {
          id: "list-a",
          name: "List A (QA)",
          mimeType: "application/vnd.google-earth.kml+xml",
          modifiedTime: "2024-10-01T10:00:00Z",
          size: String(Buffer.byteLength(fixtures["list-a"], "utf8")),
        },
        {
          id: "list-b",
          name: "List B (QA)",
          mimeType: "application/vnd.google-earth.kml+xml",
          modifiedTime: "2024-10-02T08:00:00Z",
          size: String(Buffer.byteLength(fixtures["list-b"], "utf8")),
        },
      ],
    });
    return;
  }

  if (req.method === "GET" && req.url?.startsWith("/drive/v3/files/")) {
    const id = req.url.split("/").pop()?.split("?")[0];
    const body = id && fixtures[id];
    if (body) {
      res.writeHead(200, { "content-type": "application/vnd.google-earth.kml+xml" });
      res.end(body);
    } else {
      res.writeHead(404).end("not found");
    }
    return;
  }

  res.writeHead(404);
  res.end("Unknown mock endpoint");
});

server.listen(port, () => {
  console.log(`[mock-drive] listening on http://localhost:${port}`);
  console.log("Set the following env vars before running the app:");
  console.log(`  GOOGLE_DEVICE_CODE_ENDPOINT=http://localhost:${port}/device/code`);
  console.log(`  GOOGLE_TOKEN_ENDPOINT=http://localhost:${port}/token`);
  console.log(`  GOOGLE_USERINFO_ENDPOINT=http://localhost:${port}/userinfo`);
  console.log(`  GOOGLE_DRIVE_API_BASE=http://localhost:${port}/drive/v3`);
});

process.on("SIGINT", () => {
  console.log("\n[mock-drive] shutting down");
  server.close(() => process.exit(0));
});

function respondJson(res, payload) {
  res.writeHead(200, { "content-type": "application/json" });
  res.end(JSON.stringify(payload));
}
