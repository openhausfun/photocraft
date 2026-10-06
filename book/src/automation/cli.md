# Command-line interface

`apps/photocraft-cli` provides the following current subcommands:

| Command | Purpose |
|---|---|
| `convert` | Open one document and export it to another format |
| `info` | Print document size, mode, depth, and layer information as JSON |
| `run` | Open or create a document, execute engine commands, and optionally save |
| `batch` | Apply an action list to files in one input directory |
| `droplet` | Run a PhotoCraft droplet against files or folders |
| `commands` | List command IDs and parameter documentation |
| `mcp` | Start the MCP server on stdio, optionally bridged to the desktop app |
| `serve` | Start the JSON-lines headless server on stdio or loopback TCP |

Use `photocraft-cli --help` as the executable source of truth. A typical bounded command run is:

```sh
photocraft-cli run input.psd \
  --cmd image.adjustments.invert \
  --out output.png
```

Paths supplied to the current CLI are ordinary operating-system paths. The CLI does not restrict them to a workspace root. Batch output creates the requested output directory, and save operations may create parent directories. Run untrusted action files only in an appropriately isolated account or environment.

CLI parsing reports malformed flags and JSON as errors, but it is not a security policy layer. Engine commands remain responsible for validating their own parameters.
