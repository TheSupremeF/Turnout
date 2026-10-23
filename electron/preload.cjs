// Preload for the Turnout desktop app.
//
// The renderer talks to the CLI over HTTP (`/api/turnout/*`) and needs no
// privileged API, so nothing is exposed here. The file exists to document that
// choice: if a future feature genuinely needs IPC, it belongs here and nowhere
// else, and it must validate its arguments before calling into main.

// Intentionally empty.
