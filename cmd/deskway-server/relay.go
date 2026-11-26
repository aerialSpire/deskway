package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"log"
	"os"
	"os/signal"
	"syscall"

	"deskway/internal/server"
)

const (
	relayUsage      = "deskway-server relay -join https://main.example [-token TOKEN] [-data DIR] [-public-ip IP] [-turn-port N] [-turn-min-port N] [-turn-max-port N]"
	turnDefaultPort = 3478
	turnDefaultMin  = 49160
	turnDefaultMax  = 49359
)

func relayCommand(args []string) int {
	flags := flag.NewFlagSet("relay", flag.ContinueOnError)
	join := flags.String("join", "", "HTTPS origin of the main server")
	token := flags.String("token", "", "single-use join token (first start only)")
	data := flags.String("data", "deskway-relay", "relay state directory")
	ip := flags.String("public-ip", "", "public IPv4 address (default: address seen by the main server)")
	port := flags.Int("turn-port", turnDefaultPort, "TURN port (UDP and TCP)")
	minPort := flags.Int("turn-min-port", turnDefaultMin, "lowest TURN relay port")
	maxPort := flags.Int("turn-max-port", turnDefaultMax, "highest TURN relay port")
	flags.Usage = func() {
		fmt.Fprintln(flags.Output(), "Usage: "+relayUsage)
		flags.PrintDefaults()
	}
	if err := flags.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return 0
		}
		return 2
	}
	if *join == "" || *data == "" || flags.NArg() != 0 {
		flags.Usage()
		return 2
	}
	if *port < 1 || *port > 65535 || *minPort < 1 || *maxPort > 65535 || *minPort > *maxPort {
		log.Print("TURN ports must be in 1–65535 and the range must be in order")
		return 2
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	cfg := server.TURNConfig{PublicIP: *ip, Port: *port, MinPort: uint16(*minPort), MaxPort: uint16(*maxPort)}
	if err := server.RunRelayNode(ctx, *join, *token, *data, cfg); err != nil && !errors.Is(err, context.Canceled) {
		log.Print(err)
		return 1
	}
	return 0
}
