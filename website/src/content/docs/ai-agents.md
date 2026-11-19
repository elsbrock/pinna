---
title: Use an AI agent
description: Make Hermes or OpenClaw the satellite's brain, and let AI agents use the satellite over MCP.
---

Hermes and OpenClaw are AI agents: they keep their own memory and tools. In
[standalone mode](/docs/standalone/), one of them can be the satellite's
brain, and agents can use the satellite over MCP, the protocol they use to
call tools.

## Before you begin

- Speech-to-text and text-to-speech are set up for
  [standalone mode](/docs/standalone/#add-the-services).
- The agent runs on a computer that the satellite reaches. See
  [Your own servers](/docs/standalone/#your-own-servers).

## Make an agent the brain

Add the agent under **Assistant** > **Services**, and choose it as the
**Brain**, as described in
[Add the services](/docs/standalone/#add-the-services). The satellite then
hands each request to the agent and speaks its answer.

- **Hermes** answers once its API server is on and reachable from the
  satellite. Enter its API key as the connection's **API key**, and as the
  model, a name its API server lists.
- **OpenClaw**'s chat endpoint is off until you turn it on in OpenClaw. It
  chooses its own model.

- The satellite offers an agent none of Pinna's tools and none of your MCP
  servers. To let it set timers or play radio on the satellite, give it
  Pinna's MCP address (below).
- Hermes gets the conversation so far. OpenClaw gets only the new request and
  keeps the conversation itself.
- An agent may take a while; the acknowledgment tells you it heard you. If it
  takes longer than 2 minutes to start answering, the request fails. To change
  that, select **Assistant**, then **Services**, and under **Brain** select
  **Advanced**: **Time to answer** sets the limit, and **Who runs the tools**
  and **Conversation it gets** change how the satellite treats the brain.

## Connect an agent over MCP

An agent reaches the satellite as an MCP server:

1. Select **Assistant**, then **Tools**.
2. Under **Pinna as an MCP server**, select **Copy** next to the address.
3. Enter the address in the agent's MCP settings.

The agent can then set timers, play a stream, speak a text, change the volume
and the light, mute the microphones, switch the satellite back to Home
Assistant, read the satellite's status and which version it runs, and check
for updates. It can't install an update or make the satellite listen, and it
doesn't get the weather. Like the dashboard, the MCP server has no password:
anyone on your network can use it. It answers in standalone mode only. The list under it shows the agents that called.

## What's next

- [Choose what the brain may do](/docs/standalone/#choose-what-the-brain-may-do):
  the tools that the MCP server offers follow the same switches.
- For developers: the [MCP server](/docs/control-api/#mcp-server) in the
  control API reference.
