package node_test

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"knock/internal/invite"
	"knock/internal/node"
)

func TestIdentityIsStableAcrossLoads(t *testing.T) {
	dir := t.TempDir()

	first, err := node.LoadOrCreateIdentity(dir)
	if err != nil {
		t.Fatalf("首次创建身份失败: %v", err)
	}
	second, err := node.LoadOrCreateIdentity(dir)
	if err != nil {
		t.Fatalf("再次加载身份失败: %v", err)
	}

	if first.Pin() != second.Pin() {
		t.Fatalf("pin 变了：%x != %x —— 这会让之前发出去的码全部失效", first.Pin(), second.Pin())
	}
}

func TestIdentityFileIsPemAndNotOverwritten(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, node.KeyFileName)

	first, err := node.LoadOrCreateIdentity(dir)
	if err != nil {
		t.Fatalf("创建身份失败: %v", err)
	}
	if first.Pin() == ([8]byte{}) {
		t.Fatal("pin 不应该是全零")
	}

	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("读密钥文件失败: %v", err)
	}
	if !strings.Contains(string(raw), "-----BEGIN PRIVATE KEY-----") {
		t.Fatalf("密钥文件不是 PKCS8 PEM，开头是：%q", string(raw[:40]))
	}

	// 再加载一次不能把文件重写掉
	before := string(raw)
	if _, err := node.LoadOrCreateIdentity(dir); err != nil {
		t.Fatalf("再次加载失败: %v", err)
	}
	after, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if string(after) != before {
		t.Fatal("再次加载时密钥文件被改写了")
	}
}

// Windows 上 os.FileMode 的权限位基本没有意义，只在类 Unix 上验。
func TestIdentityFilePermissions(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("Windows 不落 Unix 权限位")
	}
	dir := t.TempDir()
	if _, err := node.LoadOrCreateIdentity(dir); err != nil {
		t.Fatalf("创建身份失败: %v", err)
	}

	info, err := os.Stat(filepath.Join(dir, node.KeyFileName))
	if err != nil {
		t.Fatal(err)
	}
	if perm := info.Mode().Perm(); perm != 0o600 {
		t.Fatalf("密钥文件权限 = %#o，想要 0600", perm)
	}
	dirInfo, err := os.Stat(dir)
	if err != nil {
		t.Fatal(err)
	}
	if perm := dirInfo.Mode().Perm(); perm != 0o700 {
		t.Fatalf("数据目录权限 = %#o，想要 0700", perm)
	}
}

func TestPinMatchesPublicKeyHash(t *testing.T) {
	seed := []byte("0123456789abcdef0123456789abcdef")
	id, err := node.NewIdentityFromSeed(seed)
	if err != nil {
		t.Fatalf("构造身份失败: %v", err)
	}

	want := invite.PinFromPublicKey(id.PublicKey())
	if id.Pin() != want {
		t.Fatalf("pin = %x，想要 %x", id.Pin(), want)
	}
}

func TestNewIdentityFromSeedRejectsBadLength(t *testing.T) {
	if _, err := node.NewIdentityFromSeed([]byte("太短")); err == nil {
		t.Fatal("种子长度不对时应当报错")
	}
}
