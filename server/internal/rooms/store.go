package rooms

import "sync"

// Store 是房间的存储接口。
//
// 骨架期只有内存实现。以后接 SQLite 时**只换这一层的实现**，Service 与 HTTP 层不动。
// 注意 Store 不做过期判断——过期是领域规则，归 Service。
type Store interface {
	Put(Room) error
	Get(code string) (Room, bool)
	Delete(code string) bool
	List() []Room
	Len() int
}

// MemoryStore 是进程内存储。节点重启即清空，这对「房间是易失登记」的语义是合理的。
type MemoryStore struct {
	mu    sync.RWMutex
	rooms map[string]Room
}

func NewMemoryStore() *MemoryStore {
	return &MemoryStore{rooms: make(map[string]Room)}
}

func (s *MemoryStore) Put(r Room) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.rooms[r.Code] = r
	return nil
}

func (s *MemoryStore) Get(code string) (Room, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	r, ok := s.rooms[code]
	return r, ok
}

func (s *MemoryStore) Delete(code string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.rooms[code]; !ok {
		return false
	}
	delete(s.rooms, code)
	return true
}

func (s *MemoryStore) List() []Room {
	s.mu.RLock()
	defer s.mu.RUnlock()
	out := make([]Room, 0, len(s.rooms))
	for _, r := range s.rooms {
		out = append(out, r)
	}
	return out
}

func (s *MemoryStore) Len() int {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return len(s.rooms)
}
