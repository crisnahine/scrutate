// lgq: a minimal command-line client for livegrep's codesearch backend (gRPC), printing
// path:line:text like the other tools, so livegrep can be measured per query without its web
// frontend in between. usage: lgq [-addr host:port] [-i] [-max N] REGEX
package main

import (
	"context"
	"flag"
	"fmt"
	"os"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"

	pb "lgq/pb"
)

func main() {
	addr := flag.String("addr", "localhost:9999", "codesearch gRPC address")
	fold := flag.Bool("i", false, "case-insensitive")
	max := flag.Int("max", 100000000, "maximum number of matches")
	flag.Parse()
	if flag.NArg() != 1 {
		fmt.Fprintln(os.Stderr, "usage: lgq [-addr host:port] [-i] [-max N] REGEX")
		os.Exit(2)
	}
	conn, err := grpc.NewClient(*addr, grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithDefaultCallOptions(grpc.MaxCallRecvMsgSize(1<<30)))
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}
	defer conn.Close()
	res, err := pb.NewCodeSearchClient(conn).Search(context.Background(), &pb.Query{Line: flag.Arg(0), FoldCase: *fold, MaxMatches: int32(*max)})
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}
	w := os.Stdout
	for _, r := range res.Results {
		fmt.Fprintf(w, "%s:%d:%s\n", r.Path, r.LineNumber, r.Line)
	}
	if res.Stats.GetExitReason() != pb.SearchStats_NONE {
		fmt.Fprintf(os.Stderr, "lgq: search stopped early: %v\n", res.Stats.GetExitReason())
		os.Exit(3)
	}
	if len(res.Results) == 0 {
		os.Exit(1)
	}
}
