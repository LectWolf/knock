// 命令 knock-hub 是自托管节点的入口。
//
// 职责边界（ADR-0002 / ADR-0003）：
//   - 本进程做「房间登记 + 配额」这类业务；
//   - 中继角色交给 iroh-relay 二进制，本进程**不认识** iroh，也不该认识。
//
// 启动顺序：数据目录 → 身份（没有就生成）→ 配置（没有就写默认）→ 打印节点码 → 服务。
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"knock/internal/httpapi"
	"knock/internal/invite"
	"knock/internal/node"
	"knock/internal/ratelimit"
	"knock/internal/rooms"
)

func main() {
	var (
		dataDir    = flag.String("data-dir", defaultDataDir(), "数据目录（密钥与配置都放这里）")
		configPath = flag.String("config", "", "配置文件路径，默认 <data-dir>/node.json")
		printCode  = flag.Bool("print-code", false, "打印节点码后退出")
		addrFlag   = flag.String("addr", "", "覆盖配置里的监听地址")
		inspect    = flag.String("inspect", "", "解析一串节点码/邀请，说明它是什么，然后退出")
	)
	flag.Parse()

	// -inspect 不需要身份，也不需要数据目录，先处理它
	if *inspect != "" {
		out, err := node.Inspect(*inspect)
		if err != nil {
			fmt.Fprintf(os.Stderr, "解析失败：%v\n", err)
			os.Exit(1)
		}
		fmt.Print(out)
		return
	}

	dir, err := filepath.Abs(*dataDir)
	if err != nil {
		log.Fatalf("解析数据目录失败: %v", err)
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		log.Fatalf("创建数据目录 %s 失败: %v", dir, err)
	}

	cfgFile := *configPath
	if cfgFile == "" {
		cfgFile = filepath.Join(dir, node.ConfigFileName)
	}

	identity, err := node.LoadOrCreateIdentity(dir)
	if err != nil {
		log.Fatalf("初始化身份失败: %v", err)
	}

	cfg, err := node.LoadConfig(cfgFile)
	if err != nil {
		log.Fatalf("%v", err)
	}
	if *addrFlag != "" {
		cfg.Listen = *addrFlag
	}

	// 首次启动：把默认配置落盘，让主人有个文件可以改
	if _, statErr := os.Stat(cfgFile); errors.Is(statErr, os.ErrNotExist) {
		if err := node.WriteConfig(cfgFile, cfg); err != nil {
			log.Printf("写默认配置失败（不影响运行）: %v", err)
		} else {
			log.Printf("已生成默认配置：%s", cfgFile)
		}
	}

	if err := cfg.Validate(); err != nil {
		log.Fatalf("配置有问题：%v", err)
	}

	code, err := cfg.NodeCode(identity.Pin())
	if err != nil {
		log.Fatalf("拼节点码失败: %v", err)
	}
	codeText, err := code.Encode()
	if err != nil {
		log.Fatalf("编码节点码失败: %v", err)
	}

	// 自检：编出来的东西必须能解回同一份数据。解不回来就是代码 bug，
	// 与其把一串废码发给用户，不如当场退出。
	if back, err := invite.Decode(codeText); err != nil || back != code {
		log.Fatalf("节点码自检失败（编/解不一致，这是 bug）: %v", err)
	}

	fmt.Print(node.Banner(cfg, code, codeText))
	if *printCode {
		return
	}

	ttl, err := cfg.TTL()
	if err != nil {
		log.Fatalf("%v", err)
	}
	sweepEvery := ttl / 4
	if sweepEvery < time.Second {
		sweepEvery = time.Second
	}

	svc := rooms.NewService(rooms.NewMemoryStore(), ttl, rooms.WithMaxRooms(cfg.MaxRooms))

	// 两个限速器：查码（GET/DELETE 共用）与开门。
	// 窗口固定 1 分钟，因为配置项就叫 "…per_minute"——写明白比灵活重要。
	lookupLimiter := ratelimit.New(cfg.LookupPerMinute, time.Minute)
	createLimiter := ratelimit.New(cfg.CreatePerMinute, time.Minute)

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	// 过期清理：房间是易失的，但没人敲的门得有人关。
	go func() {
		ticker := time.NewTicker(sweepEvery)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				if n := svc.Sweep(); n > 0 {
					log.Printf("清理 %d 个过期房间，存活 %d", n, svc.Len())
				}
			}
		}
	}()

	// 限速桶也要扫：公网被打时 key 是无限的，不清会一直涨。
	go func() {
		ticker := time.NewTicker(time.Minute)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				lookupLimiter.Sweep()
				createLimiter.Sweep()
			}
		}
	}()

	api := httpapi.New(svc,
		httpapi.WithLookupLimiter(lookupLimiter),
		httpapi.WithCreateLimiter(createLimiter),
		httpapi.WithNodeID(identity.PublicKeyHex()),
		httpapi.WithName(cfg.Name),
		httpapi.WithRelayURL(cfg.RelayURL),
	)

	srv := &http.Server{
		Addr:              cfg.Listen,
		Handler:           api.Handler(),
		ReadHeaderTimeout: 5 * time.Second,
	}

	go func() {
		<-ctx.Done()
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = srv.Shutdown(shutdownCtx)
	}()

	log.Printf("接口已启动：%s（房间寿命 %s，上限 %d，查码 %d/分，开门 %d/分）",
		cfg.Listen, ttl, cfg.MaxRooms, cfg.LookupPerMinute, cfg.CreatePerMinute)
	if err := srv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
		log.Fatalf("监听失败: %v", err)
	}
	log.Print("已停止")
}

// defaultDataDir 是没指定 -data-dir 时的落点。
func defaultDataDir() string {
	if dir, err := os.UserConfigDir(); err == nil {
		return filepath.Join(dir, "knock")
	}
	return "knock-data"
}
