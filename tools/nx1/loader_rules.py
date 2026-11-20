#!/usr/bin/env python3
"""Turns decompiled `Load_<Structure>` functions of the NX1 executable into loading rules for gen_nx1_schema.py.

usage: loader_rules.py PDB_FILE DECOMPILED_DIR OUT.txt

DECOMPILED_DIR holds one `<Structure>.c` file per loader (see dump_loaders.py). The loaders of this build are generated code
with a fixed shape (a pointer member is loaded as `if (p) { if (p == -1) { p = Alloc(mask); Load...(); } else Convert(p); }`),
so each statement maps to a rule of the rule language of the zone code generator: counts, alignment, strings, blocks,
reusable pointers, conditions and the order the members are loaded in. What does not fit (loops, hand-written loaders) is
written out as a comment starting with `// TODO` for NX1_Commands.txt to answer.
"""
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pdb_types  # noqa: E402

# ---------------------------------------------------------------------------------------------------------------- lexer
TOKEN = re.compile(r"""
    (?P<num>0[xX][0-9a-fA-F]+u?|\d+u?)
  | (?P<str>"(?:[^"\\]|\\.)*")
  | (?P<id>[A-Za-z_]\w*)
  | (?P<op><<=|>>=|\+=|-=|\*=|\|=|&=|<<|>>|<=|>=|==|!=|&&|\|\||->|\+\+|--|[-+*/%&|^~!<>=?:;,.(){}\[\]])
""", re.X)


def lex(text):
    text = re.sub(r"//[^\n]*", "", text)
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    return [(m.lastgroup, m.group()) for m in TOKEN.finditer(text)]


class Unsupported(Exception):
    pass


TYPE_WORDS = {"int", "unsigned", "signed", "char", "short", "long", "float", "double", "void", "struct", "const", "_DWORD", "_BYTE",
              "_WORD", "_QWORD", "__int8", "__int16", "__int32", "__int64", "BOOL", "_BOOL1", "bool", "__fastcall"}


class Parser:
    def __init__(self, toks):
        self.t = toks
        self.p = 0

    def peek(self, o=0):
        return self.t[self.p + o][1] if self.p + o < len(self.t) else None

    def next(self):
        v = self.t[self.p][1]
        self.p += 1
        return v

    def expect(self, v):
        n = self.next()
        if n != v:
            raise Unsupported("expected %s got %s" % (v, n))

    # statements -> ("blk", [..]) ("if", cond, then, else) ("expr", e) ("ret", e) ("decl",) ("other", name)
    def block(self):
        self.expect("{")
        out = []
        while self.peek() != "}":
            out.append(self.stmt())
        self.expect("}")
        return ("blk", out)

    def is_decl(self):
        i, n = 0, 0
        while self.peek(i) in TYPE_WORDS or (self.peek(i) == "*" and n > 0):
            if self.peek(i) != "*":
                n += 1
            i += 1
        if n == 0:
            return False
        # a declaration is type words, stars, a name, then ';' (or ',' / '[')
        j = i
        while self.peek(j) == "*":
            j += 1
        return self.peek(j) is not None and re.match(r"[A-Za-z_]", self.peek(j)) is not None and self.peek(j + 1) in (";", ",", "[", "=")

    def stmt(self):
        v = self.peek()
        if v == "{":
            return self.block()
        if v == "if":
            self.next()
            self.expect("(")
            c = self.expr()
            self.expect(")")
            a = self.stmt()
            b = None
            if self.peek() == "else":
                self.next()
                b = self.stmt()
            return ("if", c, a, b)
        if v == "return":
            self.next()
            e = None if self.peek() == ";" else self.expr()
            self.expect(";")
            return ("ret", e)
        if v == "do":
            self.next()
            body = self.stmt()
            self.expect("while")
            self.expect("(")
            c = self.expr()
            self.expect(")")
            self.expect(";")
            return ("do", body, c)
        if v == "while":
            self.next()
            self.expect("(")
            c = self.expr()
            self.expect(")")
            return ("do", self.stmt(), c)
        if v in ("for", "switch", "goto") or (self.peek(1) == ":" and v not in TYPE_WORDS and re.match(r"[A-Za-z_]", v or "")):
            raise Unsupported(v)
        if self.is_decl():
            while self.next() != ";":
                pass
            return ("decl",)
        e = self.expr()
        self.expect(";")
        return ("expr", e)

    # expressions: ("n", v) ("v", name) ("d", type, e) ("o", op, a, b) ("u", op, a) ("c", name, [args]) ("as", lhs, rhs) ("cast", type, e)
    PREC = [["||"], ["&&"], ["|"], ["^"], ["&"], ["==", "!="], ["<", "<=", ">", ">="], ["<<", ">>"], ["+", "-"], ["*", "/", "%"]]

    def expr(self):
        lhs = self.binary(0)
        if self.peek() == "?":
            self.next()
            a = self.expr()
            self.expect(":")
            b = self.expr()
            return ("t", lhs, a, b)
        if self.peek() == "=":
            self.next()
            return ("as", lhs, self.expr())
        if self.peek() in ("+=", "-=", "*=", "|=", "&=", "<<=", ">>="):
            self.next()
            self.expr()
            return ("as2", lhs)
        return lhs

    def binary(self, lvl):
        if lvl == len(self.PREC):
            return self.unary()
        a = self.binary(lvl + 1)
        while self.peek() in self.PREC[lvl]:
            op = self.next()
            a = ("o", op, a, self.binary(lvl + 1))
        return a

    def type_at(self):
        """If a cast `(type *)` starts here returns its length in tokens."""
        if self.peek() != "(":
            return 0
        i = 1
        n = 0
        while self.peek(i) in TYPE_WORDS:
            i += 1
            n += 1
        if n == 0:
            return 0
        while self.peek(i) == "*":
            i += 1
        return i + 1 if self.peek(i) == ")" else 0

    def unary(self):
        v = self.peek()
        if v in ("++", "--"):
            self.next()
            return ("u", v, self.unary())
        if v in ("!", "-", "~", "&", "*", "+"):
            self.next()
            a = self.unary()
            if v == "*":
                if a[0] == "cast":
                    return ("d", a[1], a[2])
                return ("d", None, a)
            return ("u", v, a)
        n = self.type_at()
        if n:
            words = [self.t[self.p + k][1] for k in range(1, n - 1)]
            self.p += n
            return ("cast", " ".join(words), self.unary())
        return self.postfix()

    def postfix(self):
        a = self.primary()
        while True:
            v = self.peek()
            if v == "(" and a[0] == "v":
                self.next()
                args = []
                while self.peek() != ")":
                    args.append(self.expr())
                    if self.peek() == ",":
                        self.next()
                self.expect(")")
                a = ("c", a[1], args)
            elif v == "[":
                self.next()
                i = self.expr()
                self.expect("]")
                a = ("ix", a, i)
            elif v in ("++", "--"):
                self.next()
                a = ("u", v, a)
            else:
                return a

    def primary(self):
        kind, v = self.t[self.p]
        if kind == "num":
            self.p += 1
            return ("n", int(v.rstrip("uU"), 0))
        if kind == "str":
            self.p += 1
            return ("s", v)
        if kind == "id":
            self.p += 1
            return ("v", v)
        if v == "(":
            self.next()
            e = self.expr()
            self.expect(")")
            return e
        raise Unsupported("token %s" % v)


# ---------------------------------------------------------------------------------------------------------------- model
class Model:
    def __init__(self, pdb):
        self.t = pdb_types.Types(pdb)
        self.info = {}
        self.gvar = {}  # global variable name -> structure name

    def struct(self, name):
        if name not in self.info:
            if name in COPIES:
                # a structure the loaders treat separately that the program database has once (same layout, other rules)
                self.info[name] = self.struct(COPIES[name])
                self.names[name] = name
                return self.info[name]
            real = self.resolve_name(name)
            if real is None:
                return None
            self.info[name] = self.t.struct_info(real)
            self.names[name] = real
        return self.info[name]

    names = {}

    def member_at(self, sname, off, deep):
        """Name chain of the member at a byte offset of a structure. `deep` descends into an embedded structure that starts at
        that offset (for a string or pointer member that lives in a nested structure); otherwise the member itself is named
        unless the offset lies inside it."""
        s = self.struct(sname)
        if s is None or s["kind"] == "enum":
            return None
        cands = [m for m in s["members"] if m["o"] <= off < m["o"] + max(m["sz"], 1)]
        if not cands:
            return None
        exact = [m for m in cands if m["o"] == off]
        m = (exact or cands)[0]
        t = m["t"]
        rest = off - m["o"]
        if t["k"] in ("struct", "union") and (rest > 0 or deep):
            sub = self.member_at(t["n"], rest, deep)
            return [m["n"]] + sub if sub else [m["n"]]
        if t["k"] == "arr" and rest > 0 and t["n"] > 0:
            # an element of an array (of scalars or pointers; two dimensions at most)
            dims, el = [], t
            while el["k"] == "arr":
                dims.append(el["n"])
                el = el["of"]
            total = 1
            for d in dims:
                total *= d
            flat = rest // (m["sz"] // total)
            idx = []
            for d in reversed(dims):
                idx.append(flat % d)
                flat //= d
            return [m["n"] + "".join("[%d]" % i for i in reversed(idx))]
        return [m["n"]]

    def member_deep(self, sname, chain):
        cur = sname
        m = None
        for n in chain:
            m = self.member(cur, re.sub(r"(\[\d+\])+$", "", n))
            if m is None:
                return None
            nxt = struct_of_type(m["t"])
            cur = nxt if nxt else cur
        return m

    def ptr_loader_reuses(self, base):
        """Whether the pointer loader `Load_<base>` converts offsets (a reusable pointer) rather than registering an asset."""
        text = self.texts.get(base)
        return bool(text) and "DB_ConvertOffsetToPointer" in text and "DB_InsertPointer" not in text

    texts = {}

    def real(self, n):
        self.struct(n)
        return self.names.get(n, n)

    def resolve_name(self, n):
        for c in (n, n + "_s", n + "_t", n.replace("_t", "_s"), n + "Def_t", {"Window": "windowDef_t", "Font": "Font_s", "GfxVertexShaderLoadDef": "GfxShaderLoadDef", "GfxPixelShaderLoadDef": "GfxShaderLoadDef"}.get(n, n)):
            if c in self.t.defs:
                return c
        return None

    def member(self, sname, mname):
        s = self.struct(sname)
        for m in s["members"]:
            if m["n"] == mname:
                return m
        return None


# structure -> the structure whose layout it copies
COPIES = {"cbrushWrapper_t": "cbrush_t"}


def struct_of_type(t):
    while t["k"] in ("ptr", "arr", "bits"):
        t = t.get("to") or t.get("of")
    return t["n"] if t["k"] in ("struct", "union") else None


def has_pointer(model, t, seen=None):
    """Whether a member type is, or contains, a pointer."""
    seen = seen or set()
    if t["k"] == "ptr":
        return True
    if t["k"] == "arr":
        return has_pointer(model, t["of"], seen)
    if t["k"] in ("struct", "union"):
        if t["n"] in seen:
            return False
        seen.add(t["n"])
        s = model.struct(t["n"])
        return s is not None and any(has_pointer(model, m["t"], seen) for m in s["members"])
    return False


def elem_size(model, t):
    """Size of what the first pointer of a member type points at (the element of a pointer array: the pointer)."""
    while t["k"] in ("arr",):
        t = t["of"]
    if t["k"] != "ptr":
        return None
    t = t["to"]
    mult = 1
    while t["k"] == "arr":  # a pointer to an array: the element is the whole array
        mult *= t["n"]
        t = t["of"]
    if mult > 1:
        inner = elem_size(model, {"k": "ptr", "to": t})
        return inner * mult if inner else None
    if t["k"] == "ptr":
        return 4
    if t["k"] == "prim":
        return max(t["sz"], 1)
    if t["k"] in ("struct", "union"):
        s = model.struct(t["n"])
        return s["size"] if s else None
    if t["k"] == "enum":
        return model.struct(t["n"])["size"]
    return None


# ---------------------------------------------------------------------------------------------------------------- translation
BLOCK_NAMES = ["XFILE_BLOCK_TEMP", "XFILE_BLOCK_PHYSICAL", "XFILE_BLOCK_RUNTIME", "XFILE_BLOCK_VIRTUAL", "XFILE_BLOCK_LARGE", "XFILE_BLOCK_RUNTIME_PHYSICAL"]


class Rule:
    def __init__(self, chain):
        self.chain = chain
        self.string = False
        self.script = False
        self.count = None
        self.align = None
        self.reusable = False
        self.block = None
        self.conds = []
        self.todo = []

    @property
    def name(self):
        return "::".join(self.chain)


class FuncTranslator:
    def __init__(self, model, sname, tree):
        self.m = model
        self.s = sname
        self.tree = tree
        self.rules = {}
        self.order = []
        self.self_var = None
        self.alias = {}
        self.struct_block = None
        self.todo = []
        self.blk_stack = []
        self.first_load = True
        self.cur_target = None
        self.pointee = {}
        self.foreign = []

    # ----- addresses and expressions
    def resolve_base(self, e):
        """(structure name, offset) for an address expression, or None."""
        if e[0] == "v":
            if e[1] in self.alias:
                tgt = self.alias.pop(e[1])
                try:
                    return self.resolve_base(tgt)
                finally:
                    self.alias[e[1]] = tgt
            if e[1] == self.self_var:
                return (self.s, 0)
            if e[1] in self.pointee and self.pointee[e[1]]:
                return (self.pointee[e[1]], 0)
            if e[1] in self.m.gvar:
                return (self.m.gvar[e[1]], 0)
            return None
        if e[0] == "cast":
            return self.resolve_base(e[2])
        if e[0] == "o" and e[1] == "+":
            for a, b in ((e[2], e[3]), (e[3], e[2])):
                base = self.resolve_base(a)
                if base is not None and b[0] == "n":
                    return (base[0], base[1] + b[1])
        return None

    def tr(self, e):
        """Rule-language text of an integer expression."""
        k = e[0]
        if k == "n":
            return str(e[1])
        if k == "cast":
            return self.tr(e[2])
        if k == "d":
            base = self.resolve_base(e[2])
            if base is None:
                raise Unsupported("dereference %r" % (e[2],))
            sname, off = base
            chain = self.m.member_at(sname, off, True)
            if chain is None:
                raise Unsupported("no member at %s+%d" % (sname, off))
            txt = "::".join(chain)
            if sname != self.s:
                txt = self.m.real(sname) + "::" + txt
            return txt
        if k == "o":
            return "( %s %s %s )" % (self.tr(e[2]), e[1], self.tr(e[3]))
        if k == "u":
            if e[1] == "!":
                return "( %s == 0 )" % self.tr(e[2])
            if e[1] == "-":
                return "( 0 - %s )" % self.tr(e[2])
            raise Unsupported("unary " + e[1])
        if k == "v":
            if e[1] in self.alias:
                tgt = self.alias.pop(e[1])
                try:
                    return self.tr(tgt)
                finally:
                    self.alias[e[1]] = tgt
            raise Unsupported("variable " + e[1])
        raise Unsupported("expression %s" % k)

    def simplify_size(self, size_e, es):
        """Count from a byte size expression: the multiplication by the element size is dropped."""
        if size_e[0] == "n":
            if es and size_e[1] % es == 0:
                return str(size_e[1] // es)
            raise Unsupported("size %d not a multiple of %s" % (size_e[1], es))
        if size_e[0] == "o" and size_e[1] == "*":
            if size_e[2][0] == "n" and size_e[2][1] == es:
                return self.tr(size_e[3])
            if size_e[3][0] == "n" and size_e[3][1] == es:
                return self.tr(size_e[2])
            if size_e[2][0] == "n" and es and size_e[2][1] % es == 0:
                return "( %d * %s )" % (size_e[2][1] // es, self.tr(size_e[3]))
        if es == 1:
            return self.tr(size_e)
        return "( %s / %d )" % (self.tr(size_e), es or 1)

    # ----- rules
    def rule(self, chain):
        key = "::".join(chain)
        if key not in self.rules:
            self.rules[key] = Rule(chain)
            self.order.append(key)
        return self.rules[key]

    def block_now(self):
        return self.blk_stack[-1] if self.blk_stack else None

    def chain_at(self, off, deep):
        return self.m.member_at(self.s, off, deep)

    def contains_call(self, node, name):
        if isinstance(node, tuple):
            if node and node[0] == "c" and node[1] == name:
                return True
            return any(self.contains_call(x, name) for x in node)
        if isinstance(node, list):
            return any(self.contains_call(x, name) for x in node)
        return False

    def calls_in(self, node, out):
        if isinstance(node, tuple):
            if node and node[0] == "c":
                out.append(node)
            for x in node:
                self.calls_in(x, out)
        elif isinstance(node, list):
            for x in node:
                self.calls_in(x, out)
        return out

    def is_minus_one(self, c):
        return c[0] == "o" and c[1] == "==" and (c[3] == ("n", 0xFFFFFFFF) or c[3] == ("u", "-", ("n", 1)))

    def find_minus_one_if(self, body):
        """The `if (p == -1) {...} else {...}` inside the then-branch of a pointer check (or None)."""
        items = body[1] if body[0] == "blk" else [body]
        for st in items:
            if st[0] == "if" and self.is_minus_one(st[1]):
                return st
        return None

    def self_ptr_check(self, c):
        """`if (*(DWORD*)(self + off) != 0)` / `if (v[9] != 0)`: the member offset, else None."""
        if c[0] == "o" and c[1] == "!=" and c[3] == ("n", 0):
            a = c[2]
            if a[0] == "d":
                base = self.resolve_base(a[2])
                if base and base[0] == self.s:
                    return base[1]
            if a[0] == "ix":
                b = self.resolve_base(a[1])
                if b and b[0] == self.s and a[2][0] == "n":
                    return b[1] + 4 * a[2][1]
        return None

    # ----- statements
    def walk(self, node, conds):
        items = node[1] if node[0] == "blk" else [node]
        for st in items:
            self.statement(st, conds)

    def statement(self, node, conds):
        k = node[0]
        if k == "decl":
            return
        if k == "blk":
            self.walk(node, conds)
        elif k == "ret":
            if node[1] is not None:
                self.expr_stmt(node[1], conds)
        elif k == "expr":
            self.expr_stmt(node[1], conds)
        elif k == "if":
            self.if_stmt(node, conds)
        elif k == "do":
            self.todo.append("loop in the loader (members inside are loaded once per pass)")
            self.walk(node[1], conds)
        else:
            self.todo.append("unsupported statement %s" % k)

    def if_stmt(self, node, conds):
        _, c, a, b = node
        calls = self.calls_in(a, []) + (self.calls_in(b, []) if b else [])
        if calls and all(x[1].startswith("MAssert") for x in calls):
            return
        off = self.self_ptr_check(c)
        if off is not None:
            self.pointer_member(off, a, b, conds)
            return
        try:
            text = self.tr(c)
        except Unsupported as ex:
            self.todo.append("condition: %s" % ex)
            return
        self.walk(a, conds + [text])
        if b is not None:
            self.walk(b, conds + ["( %s ) == 0" % text])

    def pointer_member(self, off, then, els, conds):
        chain = self.chain_at(off, True)
        if chain is None:
            self.todo.append("pointer at offset %d has no member" % off)
            return
        r = self.rule(chain)
        r.conds = list(conds)
        r.block = self.block_now()
        mi = self.find_minus_one_if(then)
        if mi is not None:
            r.reusable = mi[3] is not None
            body = mi[2]
        else:
            r.reusable = els is not None and self.contains_call(els, "DB_ConvertOffsetToPointer")
            body = then
        mem = self.m.member_deep(self.s, chain)
        es = elem_size(self.m, mem["t"]) if mem else None
        pstruct = struct_of_type(mem["t"]) if mem else None
        for st in self.flat(body):
            if st[0] == "expr" and st[1][0] == "as" and st[1][1][0] == "v":
                name, rhs = st[1][1][1], st[1][2]
                if rhs[0] == "d":
                    # `dword_x = *(self + off)`: the variable designates what the pointer member points at
                    pb = self.resolve_base(rhs[2])
                    if pb is not None and pb[0] == self.s and pstruct:
                        self.pointee[name] = pstruct
                    continue
                pb = self.resolve_base(rhs)
                if pb is not None and name.startswith("dword_"):
                    self.cur_target = pb
                    continue
            calls = self.calls_in(st, [])
            for c in calls:
                self.body_call(r, c, es, pstruct)

    def flat(self, node):
        out = []
        if node[0] == "blk":
            for x in node[1]:
                out += self.flat(x)
        elif node[0] == "if":
            out += self.flat(node[2])
            if node[3] is not None:
                out += self.flat(node[3])
        else:
            out.append(node)
        return out

    def body_call(self, r, c, es, pstruct):
        fn, args = c[1], c[2]
        target, self.cur_target = self.cur_target, None
        if fn == "DB_AllocStreamPos":
            r.align = args[0][1] + 1 if args[0][0] == "n" else None
            self.cur_target = target
        elif fn == "Load_Stream" and args[0] == ("n", 1):
            self.cur_target = target
            try:
                r.count = self.simplify_size(args[2], es)
            except Unsupported as ex:
                r.todo.append("size: %s" % ex)
        elif fn.startswith("Load_") and fn.endswith("Array") and len(args) == 2:
            try:
                r.count = self.tr(args[1])
            except Unsupported as ex:
                r.todo.append("count: %s" % ex)
            if fn.startswith("Load_XString"):
                r.string = True
            if fn.startswith("Load_ScriptString"):
                r.script = True
        elif fn in ("Load_XString", "Load_ScriptString", "Load_ScriptStringCustom", "Load_SndAliasCustom"):
            if target is not None and target[0] != self.s:
                chain = self.m.member_at(target[0], target[1], True)
                if chain and fn in ("Load_XString", "Load_SndAliasCustom"):
                    self.foreign.append((self.m.real(target[0]), "::".join(chain), "string"))
                return
            r.string = fn in ("Load_XString", "Load_SndAliasCustom")
            r.script = fn.startswith("Load_ScriptString")
        elif fn.startswith("Load_") and len(args) == 1 and args[0] == ("n", 1):
            pass
        elif fn.startswith(("Load_", "DB_")) and fn not in ("DB_PushStreamPos", "DB_PopStreamPos", "DB_InsertPointer", "DB_ConvertOffsetToPointer", "Load_Stream"):
            r.todo.append("call %s" % fn)

    def expr_stmt(self, e, conds):
        k = e[0]
        if k == "c":
            self.call_stmt(e, conds)
        elif k == "as":
            lhs, rhs = e[1], e[2]
            if rhs[0] == "c" and rhs[1].startswith(("Load_", "DB_")) and lhs[0] == "v":
                self.call_stmt(rhs, conds)
                return
            if lhs[0] == "v":
                name = lhs[1]
                base = self.resolve_base(rhs)
                if base is not None and name.startswith("dword_"):
                    self.cur_target = base
                    self.alias[name] = rhs
                    return
                if name.startswith("dword_"):
                    if rhs[0] == "d":
                        # `dword_x = *(self + off)`: the variable now designates what that pointer member points at
                        pbase = self.resolve_base(rhs[2])
                        if pbase is not None and pbase[0] == self.s:
                            chain = self.chain_at(pbase[1], True)
                            mem = self.m.member_deep(self.s, chain) if chain else None
                            tgt = struct_of_type(mem["t"]) if mem else None
                            self.pointee[name] = tgt
                    return
                if rhs == ("v", name):
                    return
                self.alias[name] = rhs

    def call_stmt(self, c, conds):
        fn, args = c[1], c[2]
        if fn == "Load_Stream" and self.first_load:
            self.first_load = False
            return
        if fn == "DB_PushStreamPos":
            if args[0][0] == "n":
                self.blk_stack.append(args[0][1])
            return
        if fn == "DB_PopStreamPos":
            if self.blk_stack:
                self.blk_stack.pop()
            return
        if fn == "Load_Stream" or fn in ("DB_InsertPointer", "DB_AllocStreamPos", "DB_ConvertOffsetToPointer"):
            return
        target, self.cur_target = self.cur_target, None
        if fn.startswith(("Load_XString", "Load_SndAliasCustom")) and target is not None and target[0] != self.s and self.m.struct(target[0]) is not None:
            chain = self.m.member_at(target[0], target[1], True)
            if chain:
                self.foreign.append((self.m.real(target[0]), "::".join(chain), "string"))
            return
        if not fn.startswith("Load_") or target is None or target[0] != self.s:
            if fn.startswith("Load_"):
                self.todo.append("call %s" % fn)
            return
        is_str = fn.startswith(("Load_XString", "Load_ScriptString", "Load_SndAliasCustom"))
        is_ptr = fn.endswith(("Ptr", "Handle", "PtrArray", "HandleArray"))
        chain = self.chain_at(target[1], is_str or is_ptr)
        if chain is None:
            self.todo.append("%s at offset %d has no member" % (fn, target[1]))
            return
        r = self.rule(chain)
        r.conds = list(conds)
        if is_ptr:
            # an asset reference needs no rule; a pointer to a structure that the pointer loader converts is reusable
            base = re.sub(r"Array$", "", fn[len("Load_"):])
            r.reusable = self.m.ptr_loader_reuses(base)
            return
        r.block = self.block_now()
        if is_str:
            r.string = fn.startswith(("Load_XString", "Load_SndAliasCustom"))
            r.script = fn.startswith("Load_ScriptString")
            if fn.endswith("Array") and len(args) == 2:
                try:
                    r.count = self.tr(args[1])
                except Unsupported as ex:
                    r.todo.append("count: %s" % ex)
        elif fn.endswith("Array") and len(args) == 2 and args[1][0] != "n":
            r.todo.append("embedded array with a counted loop: %s" % fn)

    def run(self):
        items = list(self.tree[1] if self.tree[0] == "blk" else [self.tree])
        # a push right after the structure is loaded whose pop ends the function is the structure's own block
        pairs, stack = {}, []
        for i, st in enumerate(items):
            for c in self.calls_in(st, []):
                if c[1] == "DB_PushStreamPos":
                    stack.append(i)
                elif c[1] == "DB_PopStreamPos" and stack:
                    pairs[stack.pop()] = i
        last = len(items) - 1
        drop = set()
        for i, j in sorted(pairs.items()):
            before = [c[1] for k in range(i) for c in self.calls_in(items[k], [])]
            if j == last and all(n == "Load_Stream" for n in before):
                for c in self.calls_in(items[i], []):
                    if c[1] == "DB_PushStreamPos":
                        self.struct_block = c[2][0][1]
                        drop.add(i)
                        drop.add(j)
                break
        for i, st in enumerate(items):
            if i in drop:
                # the structure's push; its pop at the end may carry the last member's call, so only a bare pop is skipped
                if st[0] == "ret" and st[1] is not None and st[1][0] == "c" and st[1][1] == "DB_PopStreamPos":
                    continue
                if st[0] == "expr" and st[1][0] == "c" and st[1][1] == "DB_PopStreamPos":
                    continue
                if st[0] in ("expr", "ret") and st[1] is not None and st[1][0] == "c" and st[1][1] == "DB_PushStreamPos":
                    continue
                if st[0] == "expr" and st[1][0] == "as" and st[1][2][0] == "c" and st[1][2][1] in ("DB_PushStreamPos", "DB_PopStreamPos"):
                    continue
            self.statement(st, [])


def parse_func(path):
    text = open(path, encoding="utf-8").read()
    toks = lex(text)
    # drop the signature up to the first '{'
    i = [k for k, t in enumerate(toks) if t[1] == "{"][0]
    p = Parser(toks[i:])
    return p.block()


def rules_for(model, sname, tree):
    f = FuncTranslator(model, sname, tree)
    # the structure variable: the first Load_Stream(a1, var, size)
    first = find_first_stream(tree)
    if first is None:
        raise Unsupported("no Load_Stream")
    f.self_var = first[0]
    f.run()
    return f


def find_first_stream(node):
    if isinstance(node, tuple):
        if node and node[0] == "c" and node[1] == "Load_Stream" and len(node[2]) == 3:
            a = node[2][1]
            if a[0] == "v":
                return (a[1], node[2][2])
            if a[0] == "o" and a[1] == "+":
                return None
        for x in node:
            r = find_first_stream(x)
            if r:
                return r
    elif isinstance(node, list):
        for x in node:
            r = find_first_stream(x)
            if r:
                return r
    return None


def main():
    pdb, ddir, out = sys.argv[1:4]
    model = Model(pdb)
    for fn in os.listdir(ddir):
        model.texts[fn[:-2]] = open(os.path.join(ddir, fn), encoding="utf-8").read()
    files = sorted(f for f in os.listdir(ddir) if f.endswith(".c"))
    trees = {}
    failed = {}
    for f in files:
        name = f[:-2]
        try:
            trees[name] = parse_func(os.path.join(ddir, f))
        except (Unsupported, IndexError, KeyError, ValueError) as e:
            failed[name] = "parse: %s" % e
    # global variable -> structure
    for name, tree in trees.items():
        if model.struct(name) is None:
            continue
        fs = find_first_stream(tree)
        if fs and fs[0].startswith("dword_"):
            model.gvar.setdefault(fs[0], name)
    lines = ["// generated by loader_rules.py from the decompiled loaders; do not edit"]
    assets = asset_loaders(ddir)
    for sname, (push, align) in sorted(assets.items()):
        lines.append("")
        lines.append("use %s;" % model.real(sname))
        lines.append("set block %s;" % BLOCK_NAMES[push])
        lines.append("set allocalign %s %d;" % (model.real(sname), align))
    done = 0
    translated = {}
    for name in sorted(trees):
        s = model.struct(name)
        if s is None or s["kind"] == "enum":
            continue
        try:
            f = rules_for(model, name, trees[name])
        except Unsupported as e:
            failed[name] = str(e)
            continue
        if name in assets:
            f.struct_block = None
        translated[model.real(name)] = f
        emit(model, name, f, lines)
        done += 1
    # structures without a loader of their own: the pointers nobody loads are not loaded (runtime pointers, such as the
    # Direct3D objects of a vertex declaration); a member that another structure's loader reaches by name is loaded
    reached = set()
    for sname, f in translated.items():
        for r in f.rules.values():
            if len(r.chain) > 1:
                cur = sname
                for n in r.chain[:-1]:
                    m = model.member(cur, n)
                    cur = struct_of_type(m["t"]) if m else cur
                reached.add((cur, r.chain[-1]))
    seen, work = set(), [model.real(n) for n in translated] + [model.real(n) for n in assets]
    while work:
        n = work.pop()
        if n in seen or model.struct(n) is None or model.struct(n)["kind"] == "enum":
            continue
        seen.add(n)
        for m in model.struct(n)["members"]:
            t = struct_of_type(m["t"])
            if t:
                work.append(t)
    for n in sorted(seen - set(translated)):
        s = model.struct(n)
        never = [m["n"] for m in s["members"] if has_pointer(model, m["t"]) and (n, m["n"]) not in reached]
        if never:
            lines.append("")
            lines.append("use %s;" % n)
            for mn in never:
                lines.append("set condition %s never;" % mn)
    for n, why in sorted(failed.items()):
        lines.append("// FAILED %s: %s" % (n, why))
    open(out, "w", encoding="utf-8", newline="\n").write("\n".join(lines) + "\n")
    print("structures:", done, "failed:", len(failed))


def asset_loaders(ddir):
    """Structures that are assets: the block the header is loaded in and its alignment, from the pointer loaders."""
    out = {}
    for fn in os.listdir(ddir):
        text = open(os.path.join(ddir, fn), encoding="utf-8").read()
        if "DB_InsertPointer" not in text:
            continue
        m = re.search(r"DB_PushStreamPos\((\d+)\)", text)
        a = re.search(r"DB_AllocStreamPos\((\d+)\)", text)
        c = re.search(r"Load_(\w+)\(1\)", text) or re.search(r"Load_(\w+)Asset\(", text)
        if m and a and c:
            out[c.group(1)] = (int(m.group(1)), int(a.group(1)) + 1)
    return out


def emit(model, sname, f, lines):
    s = model.struct(sname)
    lines.append("")
    lines.append("use %s;" % model.real(sname))
    if f.struct_block is not None:
        lines.append("set block %s;" % BLOCK_NAMES[f.struct_block])
    for t in f.todo:
        lines.append("// TODO %s" % t)
    loaded = {re.sub(r"(\[\d+\])+", "", r.chain[0]) for r in f.rules.values()}
    for key in f.order:
        r = f.rules[key]
        name = r.name
        base = re.sub(r"(\[\d+\])+", "", name)  # an element of an array of pointers: only the count is per element
        for t in r.todo:
            lines.append("// TODO %s: %s" % (name, t))
        if r.string:
            lines.append("set string %s;" % base)
        if r.script:
            lines.append("set scriptstring %s;" % base)
        if r.reusable:
            lines.append("set reusable %s;" % base)
        if r.count is not None:
            lines.append("set count %s %s;" % (name, r.count))
        if r.align is not None:
            lines.append("set allocalign %s %d;" % (base, r.align))
        if r.block is not None and r.block != 3:
            lines.append("set block %s %s;" % (base, BLOCK_NAMES[r.block]))
        if r.conds:
            lines.append("set condition %s %s;" % (base, " && ".join("( %s )" % c for c in r.conds)))
    for st, name, what in f.foreign:
        lines.append("set %s %s::%s;" % (what, st, name))
    for m in s["members"]:
        if m["n"] not in loaded and has_pointer(model, m["t"]):
            lines.append("set condition %s never;" % m["n"])
    tops = []
    for key in f.order:
        n = re.sub(r"(\[\d+\])+", "", f.rules[key].chain[0])
        if n not in tops:
            tops.append(n)
    if tops:
        lines.append("reorder: %s;" % " ".join(tops))


if __name__ == "__main__":
    main()
