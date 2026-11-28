# S3-compatible storage and private files

Trovato can store uploads in any S3-compatible object store (AWS S3, MinIO,
Ceph RGW, DigitalOcean Spaces, Backblaze B2's S3 gateway) instead of local
disk, with no AWS SDK and no new TLS stack: the kernel signs its own requests
(SigV4) over the same `reqwest`/rustls client it already uses for every other
outbound call. This is what lets several kernel pods behind a load balancer
share upload storage with no `ReadWriteMany` volume (see
[multi-server.md](multi-server.md) for the shared-volume alternative, still
needed for the Pagefind index regardless of where uploads live).

It also adds a private file scheme: a File field marked `"private": true`
serves its files only through an access-checked route, never a guessable
public URL.

## Settings

Every S3 setting is read from the environment, never the database:

| Variable | Default | Meaning |
|---|---|---|
| `S3_ENDPOINT` | `https://s3.amazonaws.com` | The store's base URL, no trailing slash |
| `S3_REGION` | `us-east-1` | SigV4 signing region (MinIO accepts any value) |
| `S3_BUCKET` | — | Required to turn S3 on at all |
| `S3_KEY_PREFIX` | (empty) | Prepended to every key, before the `public/`/`private/` split below |
| `S3_PATH_STYLE` | `false` | `true` for MinIO and most self-hosted stores (`endpoint/bucket/key`); AWS S3 uses virtual-hosted style (`bucket.endpoint/key`) |
| `S3_ACCESS_KEY_ID` | — | Required with `S3_BUCKET` |
| `S3_SECRET_ACCESS_KEY` | — | Required with `S3_BUCKET`; never stored, never logged |
| `S3_PUBLIC_BASE_URL` | unset | Serve public files straight from a CDN or the bucket's public endpoint instead of proxying through the kernel |
| `S3_PRESIGN_TTL_SECS` | `900` | How long a presigned private-download URL is valid |
| `PUBLIC_FILE_BACKEND` | `local` | `local` or `s3` |
| `PRIVATE_FILE_BACKEND` | `local` | `local` or `s3` |
| `PRIVATE_UPLOADS_DIR` | `{UPLOADS_DIR}/private` | Only read when `PRIVATE_FILE_BACKEND=local` — must never be served by a reverse proxy or static file server |
| `PRIVATE_FILE_SERVE` | `stream` | `stream` (bytes through the kernel) or `redirect` (302 to a presigned URL — only takes effect on a backend that can presign, which only S3 does) |

`public` and `private` are independent: either can be `local` or `s3`, and
they can even point at the same bucket — the kernel keeps their objects apart
by appending `public/` or `private/` to `S3_KEY_PREFIX` itself, so a single
bucket never needs two prefixes configured by hand.

Changing nothing (no `S3_BUCKET`, no `PUBLIC_FILE_BACKEND`, no
`PRIVATE_FILE_BACKEND`) is a complete no-op: every existing local-disk
deployment keeps working exactly as it did.

## Marking a field private

In a content type's field definitions, set `"private": true` in a File
field's `settings`:

```json
{
  "field_name": "field_contract",
  "field_type": "File",
  "label": "Signed contract",
  "settings": { "private": true }
}
```

A file attached to this field is marked private immediately. Its bytes are
served only by `GET /file/{id}/download`, gated by the same access check
every file already gets (does the viewer have `view` on an item referencing
it, or are they its uploader for an unreferenced file); the generic
`/files/{path}` route and the image style route both refuse a private file
outright, so there is never a guessable URL for it, regardless of who is
asking.

If an existing file was already public when its field became private, its
row is marked private right away (serving stops immediately) but its bytes
are not moved — run `trovato files migrate` (below) to relocate them, or
upload new private files straight to the private backend by passing
`visibility: private` at upload time so no relocation is ever needed.

## Moving existing files to S3 (or back)

```sh
trovato files migrate --from local --to s3
```

Copies every file currently on the `local` backend to whichever backend
`--to` names, verifying size and a SHA-256 checksum before rewriting
`file_managed.uri`, and only deleting the source copy after that rewrite
commits. Safe to interrupt (Ctrl-C, a pod restart) and rerun: an interrupted
run just leaves some files already moved and others not; rerunning only
processes what is left, with no separate progress file to manage.

## A k3s example with MinIO

A minimal single-node MinIO deployment and the environment variables to point
Trovato at it. Adjust storage class, replica count and resource requests for
a real cluster; this is meant to be copied into a values file or kustomization,
not applied as-is to production.

```yaml
# minio.yaml — a single-node MinIO, for a small or self-hosted site.
# A production MinIO deployment should run in distributed mode across
# several nodes; see MinIO's own documentation for that topology.
apiVersion: apps/v1
kind: Deployment
metadata:
  name: minio
spec:
  replicas: 1
  selector:
    matchLabels: { app: minio }
  template:
    metadata:
      labels: { app: minio }
    spec:
      containers:
        - name: minio
          image: quay.io/minio/minio:latest
          args: ["server", "/data", "--console-address", ":9090"]
          env:
            - name: MINIO_ROOT_USER
              valueFrom:
                secretKeyRef: { name: minio-credentials, key: access-key }
            - name: MINIO_ROOT_PASSWORD
              valueFrom:
                secretKeyRef: { name: minio-credentials, key: secret-key }
          ports:
            - containerPort: 9000 # S3 API
            - containerPort: 9090 # web console
          volumeMounts:
            - { name: data, mountPath: /data }
      volumes:
        - name: data
          persistentVolumeClaim: { claimName: minio-data }
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: minio-data
spec:
  accessModes: ["ReadWriteOnce"]
  resources:
    requests: { storage: 20Gi }
---
apiVersion: v1
kind: Service
metadata:
  name: minio
spec:
  selector: { app: minio }
  ports:
    - { name: api, port: 9000, targetPort: 9000 }
    - { name: console, port: 9090, targetPort: 9090 }
```

Create the bucket once (`mc` is MinIO's own CLI, or use the web console at
port 9090):

```sh
mc alias set trovato-minio http://minio.default.svc.cluster.local:9000 \
  "$MINIO_ACCESS_KEY" "$MINIO_SECRET_KEY"
mc mb trovato-minio/trovato-uploads
```

Then, on every Trovato pod:

```sh
PUBLIC_FILE_BACKEND=s3
PRIVATE_FILE_BACKEND=s3
S3_ENDPOINT=http://minio.default.svc.cluster.local:9000
S3_REGION=us-east-1
S3_BUCKET=trovato-uploads
S3_PATH_STYLE=true          # MinIO: path-style, not virtual-hosted
S3_ACCESS_KEY_ID=<from the same secret MinIO reads>
S3_SECRET_ACCESS_KEY=<from the same secret MinIO reads>
```

With both schemes on `s3` and no `S3_PUBLIC_BASE_URL`, every file (public and
private alike) is proxied through the kernel's own routes; set
`S3_PUBLIC_BASE_URL` to a CDN or MinIO's own public endpoint to serve public
files directly instead, and `PRIVATE_FILE_SERVE=redirect` to redirect a
private download to a presigned URL rather than streaming it through the
kernel.
