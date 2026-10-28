// Package node 是节点自己的东西：身份（密钥）、配置、以及由两者拼出的节点码。
//
// 与 `rooms` 的分工：rooms 是「房间登记」这条业务，node 是「这台机器是谁」这件事。
// 节点码里的 pin 来自这里的密钥，房间码里的房间属于这里的节点——两者不要混。
package node

import (
	"crypto/ed25519"
	"crypto/x509"
	"encoding/hex"
	"encoding/pem"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"

	"knock/internal/invite"
)

// KeyFileName 是身份密钥在数据目录里的文件名。
const KeyFileName = "node.key"

// Identity 是节点的长期身份。私钥只落盘、不出进程。
type Identity struct {
	priv ed25519.PrivateKey
}

// LoadOrCreateIdentity 读取已有的密钥；没有就生成一个并落盘（目录 0700、文件 0600）。
//
// 密钥决定了节点码里的 pin：**丢了它，之前发出去的码就全部对不上**。
// 所以这里宁可报错也不覆盖已有文件。
func LoadOrCreateIdentity(dir string) (*Identity, error) {
	path := filepath.Join(dir, KeyFileName)

	data, err := os.ReadFile(path)
	switch {
	case err == nil:
		return parseIdentity(data)
	case errors.Is(err, fs.ErrNotExist):
		// 继续，下面创建
	default:
		return nil, fmt.Errorf("读密钥失败 %s: %w", path, err)
	}

	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, fmt.Errorf("创建数据目录 %s: %w", dir, err)
	}

	_, priv, err := ed25519.GenerateKey(nil)
	if err != nil {
		return nil, fmt.Errorf("生成密钥: %w", err)
	}

	der, err := x509.MarshalPKCS8PrivateKey(priv)
	if err != nil {
		return nil, fmt.Errorf("序列化密钥: %w", err)
	}
	block := pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: der})

	// O_EXCL：两个进程同时首次启动时，后来的那个必须失败，不能覆盖
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		if errors.Is(err, fs.ErrExist) {
			// 竞态：别人刚写好，直接读它的
			data, err := os.ReadFile(path)
			if err != nil {
				return nil, err
			}
			return parseIdentity(data)
		}
		return nil, fmt.Errorf("写密钥 %s: %w", path, err)
	}
	defer f.Close()
	if _, err := f.Write(block); err != nil {
		return nil, fmt.Errorf("写密钥 %s: %w", path, err)
	}
	// 有些文件系统上 O_CREATE 的 mode 不生效，补一刀
	_ = os.Chmod(path, 0o600)

	return &Identity{priv: priv}, nil
}

// NewIdentityFromSeed 用给定种子构造身份，只给测试用。
func NewIdentityFromSeed(seed []byte) (*Identity, error) {
	if len(seed) != ed25519.SeedSize {
		return nil, fmt.Errorf("种子必须是 %d 字节，收到 %d", ed25519.SeedSize, len(seed))
	}
	return &Identity{priv: ed25519.NewKeyFromSeed(seed)}, nil
}

func parseIdentity(data []byte) (*Identity, error) {
	block, _ := pem.Decode(data)
	if block == nil {
		return nil, errors.New("密钥文件不是 PEM")
	}
	key, err := x509.ParsePKCS8PrivateKey(block.Bytes)
	if err != nil {
		return nil, fmt.Errorf("解析 PKCS8: %w", err)
	}
	priv, ok := key.(ed25519.PrivateKey)
	if !ok {
		return nil, errors.New("密钥文件不是 Ed25519")
	}
	return &Identity{priv: priv}, nil
}

// PublicKey 返回公钥。节点码里的 pin 就是它。
func (i *Identity) PublicKey() ed25519.PublicKey {
	return i.priv.Public().(ed25519.PublicKey)
}

// Pin 是节点码里那 8 字节：公钥 SHA-256 的前 8 字节。
func (i *Identity) Pin() [invite.PinLen]byte {
	return invite.PinFromPublicKey(i.PublicKey())
}

// PublicKeyHex 是节点公钥的 hex 形式（64 个字符）。
//
// 会出现在房间响应里供客户端验 pin——客户端拿它算 SHA-256 前 8 字节，
// 和节点码里的 pin 比对，对不上就拒绝连接。
func (i *Identity) PublicKeyHex() string {
	return hex.EncodeToString(i.PublicKey())
}
