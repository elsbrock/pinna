import { mount } from "svelte";

import "./app.css";
import App from "./App.svelte";
import { device } from "./lib/device.svelte.ts";

device.start();
mount(App, { target: document.getElementById("app")! });
