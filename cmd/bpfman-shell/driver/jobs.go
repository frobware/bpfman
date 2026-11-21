// Job-leak policy for scripts and 'bpfman-shell FILE' contracts
// that treat a leaked job as a failure.

package driver

import (
	"errors"
	"strings"
	"syscall"

	"github.com/bpfman/bpfman/cmd/bpfman-shell/shell/runtime"
	"github.com/bpfman/bpfman/cmd/internal/cli"
)

// StrictJobLeakHandler is the script-mode policy: an unmanaged
// job is a contract violation. Renders '[job] FAIL <origin>:
// never waited or killed: argv', SIGKILLs the process group so
// 'bpfman-shell script.bpfman' never leaves stray processes,
// and bumps the session leak counter so the caller surfaces a
// non-zero exit. Scripts are a reproducible test contract;
// leaking a job is a bug worth failing the run for.
//
// ESRCH means the process group has already exited. Other signal
// errors are reported alongside the leak.
func StrictJobLeakHandler(cli *cli.CLI, session *runtime.Session) func(*runtime.Job) {
	return func(j *runtime.Job) {
		origin := j.Origin
		if origin == "" {
			origin = "<stdin>"
		}
		argv := strings.Join(j.Args, " ")
		_ = cli.PrintErrf("[job] FAIL %s: never waited or killed: %s\n", origin, argv) //nolint:errcheck // This diagnostic is best effort.
		if j.PID > 0 {
			if err := syscall.Kill(-j.PID, syscall.SIGKILL); err != nil && !errors.Is(err, syscall.ESRCH) {
				_ = cli.PrintErrf("[job] cleanup: kill process group %d: %v\n", j.PID, err) //nolint:errcheck // This diagnostic is best effort.
			}
		}
		session.RecordJobLeak()
	}
}
