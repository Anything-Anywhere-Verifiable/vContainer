# Verify and share

Before removing a session, you can recheck it and export a verification policy. Replace `SESSION` below with its name, such as `demo`, `public-review` or `private-review`. The policy records the expected boot measurements, owner public key, instance and exact container launch; it contains no secret values:

```sh
vcontainer verify --session SESSION --export-policy session-policy.json
```

Give the policy and the VM's public IP, printed by `run`, to someone who wants to check the deployment. They need a trusted vContainer CLI and network access to port 7700, but neither your private key nor your Google Cloud account.

New sessions allow incoming connections only from the creator's public IP. To include another verifier, run `vcontainer run --session SESSION --allow-from CIDR` with a CIDR that includes both your own public IP and the verifier's. Keep your own IP in that range so the CLI can still reach the VM. This also changes access to any published application ports. Exporting a policy does not change network access.

Once access is ready, the verifier runs this on their machine, replacing `VM_IP` with the VM's public IP:

```sh
vcontainer verify --url "http://VM_IP:7700" --policy session-policy.json
```

The verifier must review and trust the policy's expected values. A matching image digest identifies the image that ran; it does not establish that the image was built from the published source or that the agent handles data safely. See [Security and limitations](security.md).

When you are done, remove the session. The VM keeps billing after the container exits:

```sh
vcontainer rm --session SESSION
```

