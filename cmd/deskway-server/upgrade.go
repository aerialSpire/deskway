package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"strings"

	"deskway/internal/server"
)

const upgradeUsage = "deskway-server upgrade [-to TAG]"

type upgradeCatalog struct {
	tag, installed, source, directory, failure string
}

var upgradeCatalogs = map[string]upgradeCatalog{
	"en": {
		"Choose a GitHub release tag. Defaults to the latest release.",
		"Installed %s. Restart the Deskway service to use it.\n",
		"This server was built from source. Download and install a released binary to use upgrade.",
		"Run upgrade with permission to write to the executable's directory.",
		"Check the error below, then retry or install the release by hand.",
	},
	"zh-Hans": {
		"指定 GitHub 发布标签。默认使用最新版本。",
		"已安装 %s。重启 Deskway 服务以使用此版本。\n",
		"此服务器是从源码构建的。请下载并安装已发布的程序，再使用 upgrade。",
		"请使用有权写入程序所在目录的账号运行 upgrade。",
		"请检查以下错误后重试，或手动安装发布版本。",
	},
	"zh-Hant": {
		"指定 GitHub 發布標籤。預設使用最新版本。",
		"已安裝 %s。請重新啟動 Deskway 服務以使用此版本。\n",
		"此伺服器由原始碼建置。請下載並安裝已發布的程式，再使用 upgrade。",
		"請使用有權寫入程式所在資料夾的帳號執行 upgrade。",
		"請檢查下列錯誤後重試，或手動安裝發布版本。",
	},
}

func upgradeLanguage() string {
	for _, key := range []string{"LC_ALL", "LC_MESSAGES", "LANG"} {
		if locale := os.Getenv(key); locale != "" {
			locale = strings.ReplaceAll(strings.ToLower(locale), "_", "-")
			for _, traditional := range []string{"zh-hant", "zh-tw", "zh-hk", "zh-mo"} {
				if strings.HasPrefix(locale, traditional) {
					return "zh-Hant"
				}
			}
			if strings.HasPrefix(locale, "zh") {
				return "zh-Hans"
			}
			break
		}
	}
	return "en"
}

func upgradeCommand(args []string) int {
	msg := upgradeCatalogs[upgradeLanguage()]
	flags := flag.NewFlagSet("upgrade", flag.ContinueOnError)
	flags.SetOutput(io.Discard)
	tag := flags.String("to", "", msg.tag)
	err := flags.Parse(args)
	if errors.Is(err, flag.ErrHelp) {
		fmt.Fprintln(os.Stdout, upgradeUsage)
		fmt.Fprintln(os.Stdout, msg.tag)
		return 0
	}
	if err != nil || flags.NArg() != 0 || (flags.NFlag() != 0 && *tag == "") {
		fmt.Fprintln(os.Stderr, upgradeUsage)
		fmt.Fprintln(os.Stderr, msg.tag)
		return 2
	}
	installed, err := server.Upgrade(context.Background(), *tag)
	switch {
	case errors.Is(err, server.ErrUpgradeSourceBuild):
		fmt.Fprintln(os.Stderr, msg.source)
	case errors.Is(err, server.ErrUpgradeDirectory):
		fmt.Fprintln(os.Stderr, msg.directory)
		fmt.Fprintln(os.Stderr, err)
	case err != nil:
		fmt.Fprintln(os.Stderr, msg.failure)
		fmt.Fprintln(os.Stderr, err)
	default:
		fmt.Printf(msg.installed, installed)
		return 0
	}
	return 1
}
