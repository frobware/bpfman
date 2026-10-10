package scriptmeta

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestCasesPreserveTypedInput(t *testing.T) {
	t.Parallel()
	cases, err := ParseCases("test", `[{"name":"drop-stop","params":{"fallback":1,"enabled":true,"mask":"pass,dispatcher_return"}}]`)
	if err != nil {
		t.Fatal(err)
	}
	if len(cases) != 1 || cases[0].Name != "drop-stop" {
		t.Fatalf("unexpected cases: %#v", cases)
	}
	encoded, err := json.Marshal(cases[0].Params)
	if err != nil {
		t.Fatal(err)
	}
	if string(encoded) != `{"enabled":true,"fallback":1,"mask":"pass,dispatcher_return"}` {
		t.Fatalf("input types changed: %s", encoded)
	}
}

func TestCasesRejectInvalidAdmission(t *testing.T) {
	t.Parallel()
	for _, input := range []string{
		`[]`, `null`, `{}`, `[{"name":"x","params":null}]`,
		`[{"name":"","params":{}}]`, `[{"name":"a/b","params":{}}]`,
		`[{"name":"a b","params":{}}]`,
		`[{"name":"x","params":{}},{"name":"x","params":{}}]`,
		`[{"name":"x","params":{},"extra":true}]`,
		`[{"name":"x","params":{}}] {}`, `[{"name":"x","params":{}}] garbage`,
		`[{"name":"x","params":[]}]`,
	} {
		t.Run(input, func(t *testing.T) {
			t.Parallel()
			if _, err := ParseCases("fixture", input); err == nil {
				t.Fatalf("accepted invalid cases: %s", input)
			}
		})
	}
	cases := make([]Case, 65)
	encoded, err := json.Marshal(cases)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := ParseCases("fixture", string(encoded)); err == nil {
		t.Fatal("accepted unbounded cases")
	}
}

func TestReadCasesAndLabels(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "cases.bpfman")
	header := "#pragma labels={\"rust\":\"ok\"}\n#pragma cases=[{\"name\":\"case\",\"params\":{}}]\n"
	if err := os.WriteFile(path, []byte(header), 0600); err != nil {
		t.Fatal(err)
	}
	mode, err := Read(path)
	if err != nil {
		t.Fatal(err)
	}
	if mode.Labels["rust"] != "ok" || len(mode.Cases) != 1 {
		t.Fatalf("lost metadata: %#v", mode)
	}
	if err := os.WriteFile(path, []byte(header+strings.Split(header, "\n")[1]+"\n"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := Read(path); err == nil {
		t.Fatal("accepted repeated cases pragma")
	}
}
