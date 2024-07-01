/*
Copyright 2022.

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
*/

package configMgmt

import (
	"context"
	"fmt"
	"log"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/credentials/insecure"
)

const (
	DefaultPath = "/run/bpfman-sock/bpfman.sock"
)

func CreateConnection(ctx context.Context) (*grpc.ClientConn, error) {
	var (
		addr        string
		local_creds credentials.TransportCredentials
	)

	addr = fmt.Sprintf("unix://%s", DefaultPath)
	local_creds = insecure.NewCredentials()

	conn, err := grpc.NewClient(addr, grpc.WithTransportCredentials(local_creds))
	if err == nil {
		return conn, nil
	}
	log.Printf("did not connect: %v", err)

	return nil, fmt.Errorf("unable to establish connection")
}
