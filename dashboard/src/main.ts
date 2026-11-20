import { mount } from "svelte";

import "./app.css";
import Root from "./Root.svelte";
import { device } from "./lib/device.svelte.ts";
import { security } from "./lib/security.svelte.ts";

device.start();
void security.refresh();
mount(Root, { target: document.getElementById("app")! });
