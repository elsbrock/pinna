// The Vite plugin behind `npm run dev:mock`: it answers the dashboard's API
// from mock/backend.ts instead of proxying to a satellite. It lives outside
// src/ and vite.config.ts loads it only in mock mode, so no build contains it.
import { connectNodeAdapter } from "@connectrpc/connect-node";
import type { Plugin } from "vite";

import { AssistantService } from "../src/gen/pinna/v1/assistant_pb.ts";
import { DeviceService } from "../src/gen/pinna/v1/device_pb.ts";
import { PeopleService } from "../src/gen/pinna/v1/people_pb.ts";
import { assistantImpl, deviceImpl, peopleImpl } from "./backend.ts";

export function mockBackend(): Plugin {
  return {
    name: "pinna-mock-backend",
    configureServer(server) {
      const api = connectNodeAdapter({
        routes(router) {
          router.service(DeviceService, deviceImpl);
          router.service(AssistantService, assistantImpl);
          router.service(PeopleService, peopleImpl);
        },
      });
      // Not `use(path, ...)`: it strips the prefix, and the adapter routes on the full path.
      server.middlewares.use((req, res, next) => {
        if (req.url?.startsWith("/pinna.v1.")) return api(req, res);
        // No password is ever set, so a sign-in or sign-out always succeeds.
        if (req.method === "POST" && (req.url === "/auth/login" || req.url === "/auth/logout")) {
          res.statusCode = 204;
          return res.end();
        }
        next();
      });
    },
  };
}
