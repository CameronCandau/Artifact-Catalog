# Builds

Containerized builders are intentionally deferred from the V1 critical path.

The current workflow assumes you either:

- add local artifacts you already have on disk, or
- download/build them elsewhere and then ingest them with `scripts/add-artifact.sh`

## Future Direction

The intended future model is:

- one build spec per tool under `builds/specs/`
- containerized local builds via `docker build`
- optional CI builds that emit a retained artifact
- artifact ingestion back into the catalog using the same manifest/checksum flow

That keeps build provenance separate from operator-side refresh and serving.

## Example Future Spec

```text
builds/
└── specs/
    └── sweetpotato/
        ├── Dockerfile
        ├── build.sh
        └── README.md
```
