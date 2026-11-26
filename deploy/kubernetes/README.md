# Example Kubernetes manifests

Plain YAML, no Helm: a migration Job (`migrate-job.yaml`), the servers
(`deployment.yaml`) and a Service (`service.yaml`). They are examples to copy and
adapt, not a supported chart. The upgrade procedure they implement is in
[docs/deployment/upgrades.md](../../docs/deployment/upgrades.md); the design is
[docs/design/upgrades-and-probes.md](../../docs/design/upgrades-and-probes.md).

The image tag `0.105.0` is a placeholder: replace it with the release you
deploy, in both files.

What you supply:

- a Secret named `trovato` holding at least `DATABASE_URL`, `REDIS_URL`,
  `SITE_URL`, `TRUSTED_PROXIES` (your ingress), and optionally `HEALTH_TOKEN`;
- a `ReadWriteMany` PersistentVolumeClaim named `trovato-shared`, mounted at `/shared` for uploads and the Pagefind index;
- an Ingress or load balancer in front of the Service.

Validate them (CI does this on every pull request):

```sh
kubeconform -strict -summary deploy/kubernetes/
```
