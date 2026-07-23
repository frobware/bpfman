# Fedora nested-VM harness: virtiofs create-EINVAL (resolved)

## Symptom

Creating a **new** file or directory on the virtiofs share failed with
`EINVAL`; overwriting an existing file, reads, and guest-local writes
all worked. So it was specifically new-inode creation that failed.

## Root cause

Unprivileged virtiofsd sandboxes itself in a user namespace whose
uid/gid maps contain exactly one entry each: the invoking user's own
uid and gid (verified on this host: `uid_map` = `1000 1000 1`,
`gid_map` = `100 100 1`).

On inode-creating FUSE operations (create, mkdir, mknod, symlink),
virtiofsd switches credentials to the caller's guest uid/gid so the
new inode gets the right owner. `setresuid`/`setresgid` to an id that
has no mapping in the user namespace fails with `EINVAL`, and that
errno is returned to the guest. Non-creating operations do no such
switch, which is why reads and overwrites worked.

The harness's cloud-init created the guest user with a matching uid
but no `primary_group`, so Fedora gave it a fresh group with gid 1000.
Every create therefore carried gid 1000 -- unmapped in a namespace
that maps only gid 100 -- and EINVALed. Anything run under sudo
carried 0:0 and failed the same way.

This also resolves the paradox that drove the earlier investigation:
the reference `bpfman-dev-qemu` (frobware/bpfman-hacks) creates files
fine with a byte-identical virtiofsd invocation because its cloud-init
sets `primary_group: $(id -gn)` -- `users`, gid 100 in the Fedora
guest, exactly the one mapped gid. The discriminator was never the
qemu command line, and SELinux (disproven three ways during the
investigation: permissive, unlabelled context mount, fully disabled)
was innocent throughout; the disable-and-reboot step the harness
carried was a workaround for that misdiagnosis.

## Fix

`hack/fedora-vm.sh` now passes

```
--translate-uid squash-guest:0:<host-uid>:4294967295
--translate-gid squash-guest:0:<host-gid>:4294967295
```

to virtiofsd. All guest uids/gids are squashed to the host user's own
before any credential switch, so the switch is a no-op that cannot hit
an unmapped id. This covers every guest identity -- the cloud-init
user with its fresh group, and root under sudo (which the lsm e2e
needs) -- where the reference's `primary_group` trick covered only the
interactive user. Files created on the share land host-side owned by
the invoking user.

With the real cause fixed, the SELinux disable-and-reboot step is
gone (permissive via `setenforce 0` is retained; the harness now
waits on `cloud-init status --wait` instead of the reboot), and the
`GOTMPDIR` rename-instead-of-copy workaround in `hack/lsm-e2e.sh` is
removed.

Verified in-guest: create as the user and under sudo both succeed;
host-side ownership of the created files is `<uid>:<gid>` of the
invoking user.

## Note

The Fedora 42 cloud image moved to the archive; the default
`FEDORA_IMAGE_URL` now points at
`archives.fedoraproject.org/pub/archive/...` (the
`dl.fedoraproject.org` releases path 404s).
