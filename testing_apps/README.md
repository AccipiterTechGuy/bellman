# Testing apps

Small, self-contained applications that integrate with Bellman the way a
**third party would** — over the documented JSON file protocol
(`docs/INTEGRATION.md` → *Connect your own application*), with no imports
from `bellman-core`, no database access, and no privileged knowledge.

That constraint is the point. If one of these needs something the protocol
does not document, the protocol is wrong — not the app.

| app | audience | what it demonstrates |
|---|---|---|
| [`lightbulb/`](lightbulb/) | developers | the thing you **copy**: ~130 lines of stdlib Python, terminal only, whose six-line `reply()` is the whole contract |
| `lightbulb_gui/` *(planned — DEMO1)* | everyone else | the thing you **watch**: set a time in a window, see the bulb light, see the four-state handshake |

Both stay. They serve different people: one is a snippet you lift into your
own application, the other is a demonstration you show someone who has never
heard of Bellman. Neither is a library, and they deliberately share no code —
each must stand alone to be worth copying.

Every app here needs a running Bellman (the desktop app, or any process
driving the same store) and the path to its slots root. See each app's
README.
