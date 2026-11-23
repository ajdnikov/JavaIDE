//! Code completion: keywords, snippets, JDK classes and members, and symbols
//! indexed from the project's own sources. Heuristic, fast, and fully offline.

use crate::highlight::{KEYWORDS, PRIMITIVES, is_ident};
use regex::Regex;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::LazyLock;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Kind {
    Snippet,
    Method,
    Field,
    Variable,
    Class,
    Keyword,
}

impl Kind {
    pub fn badge(self) -> &'static str {
        match self {
            Kind::Snippet => "S",
            Kind::Method => "m",
            Kind::Field => "f",
            Kind::Variable => "v",
            Kind::Class => "C",
            Kind::Keyword => "k",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub label: String,
    /// Text to insert; `$0` marks the final cursor position.
    pub insert: String,
    pub detail: String,
    pub kind: Kind,
}

impl Item {
    fn new(label: &str, insert: &str, detail: &str, kind: Kind) -> Item {
        Item { label: label.into(), insert: insert.into(), detail: detail.into(), kind }
    }

    fn method(name: &str, params: &str, ret: &str) -> Item {
        let insert = if params.trim().is_empty() { format!("{name}()$0") } else { format!("{name}($0)") };
        Item { label: name.into(), insert, detail: format!("({params}) : {ret}"), kind: Kind::Method }
    }
}

// ---------------------------------------------------------------- JDK tables

/// Members are `name(params):ret` (methods) or `name:type` (fields), `;`-separated.
const LIB: &[(&str, &str)] = &[
    ("Object", "equals(Object o):boolean;hashCode():int;toString():String;getClass():Class<?>"),
    ("String", "length():int;charAt(int index):char;isEmpty():boolean;isBlank():boolean;substring(int begin, int end):String;indexOf(String str):int;lastIndexOf(String str):int;contains(CharSequence s):boolean;startsWith(String prefix):boolean;endsWith(String suffix):boolean;equals(Object o):boolean;equalsIgnoreCase(String s):boolean;compareTo(String s):int;toUpperCase():String;toLowerCase():String;trim():String;strip():String;split(String regex):String[];replace(CharSequence a, CharSequence b):String;replaceAll(String regex, String repl):String;matches(String regex):boolean;toCharArray():char[];chars():IntStream;lines():Stream<String>;repeat(int count):String;formatted(Object... args):String;format(String fmt, Object... args):String;valueOf(Object o):String;join(CharSequence sep, CharSequence... parts):String;hashCode():int"),
    ("StringBuilder", "append(Object o):StringBuilder;insert(int offset, Object o):StringBuilder;reverse():StringBuilder;toString():String;length():int;charAt(int index):char;setCharAt(int index, char c):void;deleteCharAt(int index):StringBuilder;delete(int start, int end):StringBuilder;indexOf(String s):int;replace(int start, int end, String s):StringBuilder;setLength(int len):void;isEmpty():boolean"),
    ("List", "add(E e):boolean;get(int index):E;set(int index, E e):E;remove(int index):E;size():int;isEmpty():boolean;contains(Object o):boolean;indexOf(Object o):int;clear():void;forEach(Consumer action):void;stream():Stream<E>;sort(Comparator c):void;addAll(Collection c):boolean;removeIf(Predicate filter):boolean;iterator():Iterator<E>;subList(int from, int to):List<E>;toArray():Object[];getFirst():E;getLast():E;addFirst(E e):void;addLast(E e):void;removeFirst():E;removeLast():E;reversed():List<E>;of(E... elements):List<E>;copyOf(Collection c):List<E>"),
    ("Map", "put(K key, V value):V;get(Object key):V;getOrDefault(Object key, V def):V;containsKey(Object key):boolean;containsValue(Object v):boolean;remove(Object key):V;size():int;isEmpty():boolean;keySet():Set<K>;values():Collection<V>;entrySet():Set<Map.Entry<K,V>>;forEach(BiConsumer action):void;putIfAbsent(K key, V value):V;computeIfAbsent(K key, Function f):V;compute(K key, BiFunction f):V;merge(K key, V value, BiFunction f):V;clear():void;of(K k, V v):Map<K,V>;entry(K k, V v):Map.Entry<K,V>"),
    ("Set", "add(E e):boolean;remove(Object o):boolean;contains(Object o):boolean;size():int;isEmpty():boolean;clear():void;stream():Stream<E>;forEach(Consumer action):void;addAll(Collection c):boolean;retainAll(Collection c):boolean;removeAll(Collection c):boolean;iterator():Iterator<E>;of(E... elements):Set<E>"),
    ("Deque", "push(E e):void;pop():E;peek():E;offer(E e):boolean;poll():E;addFirst(E e):void;addLast(E e):void;pollFirst():E;pollLast():E;peekFirst():E;peekLast():E;size():int;isEmpty():boolean;clear():void"),
    ("Queue", "offer(E e):boolean;poll():E;peek():E;add(E e):boolean;remove():E;size():int;isEmpty():boolean;clear():void"),
    ("Scanner", "nextInt():int;nextLine():String;next():String;nextDouble():double;nextLong():long;nextBoolean():boolean;hasNext():boolean;hasNextInt():boolean;hasNextLine():boolean;close():void"),
    ("Math", "abs(int a):int;max(int a, int b):int;min(int a, int b):int;pow(double a, double b):double;sqrt(double a):double;cbrt(double a):double;random():double;floor(double a):double;ceil(double a):double;round(double a):long;sin(double a):double;cos(double a):double;tan(double a):double;log(double a):double;log10(double a):double;hypot(double x, double y):double;floorDiv(int a, int b):int;floorMod(int a, int b):int;clamp(long v, int min, int max):int;toRadians(double deg):double;PI:double;E:double"),
    ("System", "out:PrintStream;err:PrintStream;in:InputStream;currentTimeMillis():long;nanoTime():long;exit(int status):void;arraycopy(Object src, int srcPos, Object dest, int destPos, int length):void;getProperty(String key):String;getenv(String name):String;lineSeparator():String"),
    ("PrintStream", "println(Object x):void;print(Object x):void;printf(String format, Object... args):PrintStream;format(String format, Object... args):PrintStream;flush():void"),
    ("IO", "println(Object obj):void;println():void;print(Object obj):void;readln(String prompt):String;readln():String"),
    ("Integer", "parseInt(String s):int;valueOf(int i):Integer;toString(int i):String;toBinaryString(int i):String;toHexString(int i):String;compare(int x, int y):int;sum(int a, int b):int;max(int a, int b):int;min(int a, int b):int;intValue():int;MAX_VALUE:int;MIN_VALUE:int"),
    ("Long", "parseLong(String s):long;valueOf(long l):Long;toString(long l):String;compare(long x, long y):int;MAX_VALUE:long;MIN_VALUE:long"),
    ("Double", "parseDouble(String s):double;valueOf(double d):Double;toString(double d):String;compare(double a, double b):int;isNaN(double v):boolean;MAX_VALUE:double;MIN_VALUE:double;POSITIVE_INFINITY:double;NEGATIVE_INFINITY:double"),
    ("Boolean", "parseBoolean(String s):boolean;valueOf(boolean b):Boolean;TRUE:Boolean;FALSE:Boolean"),
    ("Character", "isDigit(char c):boolean;isLetter(char c):boolean;isLetterOrDigit(char c):boolean;isUpperCase(char c):boolean;isLowerCase(char c):boolean;isWhitespace(char c):boolean;toUpperCase(char c):char;toLowerCase(char c):char;getNumericValue(char c):int"),
    ("Arrays", "sort(Object[] a):void;toString(Object[] a):String;deepToString(Object[] a):String;asList(T... a):List<T>;fill(Object[] a, Object v):void;copyOf(T[] a, int len):T[];copyOfRange(T[] a, int from, int to):T[];stream(T[] a):Stream<T>;equals(Object[] a, Object[] b):boolean;binarySearch(Object[] a, Object key):int"),
    ("Collections", "sort(List list):void;reverse(List list):void;shuffle(List list):void;max(Collection c):T;min(Collection c):T;unmodifiableList(List list):List<T>;emptyList():List<T>;swap(List list, int i, int j):void;frequency(Collection c, Object o):int"),
    ("Stream", "filter(Predicate p):Stream<T>;map(Function f):Stream<R>;mapToInt(ToIntFunction f):IntStream;flatMap(Function f):Stream<R>;forEach(Consumer action):void;collect(Collector c):R;toList():List<T>;reduce(T identity, BinaryOperator op):T;count():long;sorted():Stream<T>;distinct():Stream<T>;limit(long n):Stream<T>;skip(long n):Stream<T>;anyMatch(Predicate p):boolean;allMatch(Predicate p):boolean;noneMatch(Predicate p):boolean;findFirst():Optional<T>;min(Comparator c):Optional<T>;max(Comparator c):Optional<T>;of(T... values):Stream<T>"),
    ("IntStream", "range(int start, int end):IntStream;rangeClosed(int start, int end):IntStream;sum():int;average():OptionalDouble;max():OptionalInt;min():OptionalInt;count():long;boxed():Stream<Integer>;map(IntUnaryOperator f):IntStream;filter(IntPredicate p):IntStream;forEach(IntConsumer action):void;toArray():int[]"),
    ("Collectors", "toList():Collector;toSet():Collector;toMap(Function k, Function v):Collector;joining(CharSequence sep):Collector;groupingBy(Function f):Collector;counting():Collector;partitioningBy(Predicate p):Collector"),
    ("Optional", "isPresent():boolean;isEmpty():boolean;get():T;orElse(T other):T;orElseGet(Supplier s):T;orElseThrow():T;ifPresent(Consumer action):void;map(Function f):Optional<U>;of(T value):Optional<T>;ofNullable(T value):Optional<T>;empty():Optional<T>"),
    ("Thread", "start():void;join():void;sleep(long millis):void;run():void;interrupt():void;isAlive():boolean;currentThread():Thread;ofVirtual():Thread.Builder;startVirtualThread(Runnable task):Thread"),
    ("Files", "readString(Path p):String;writeString(Path p, CharSequence s):Path;readAllLines(Path p):List<String>;lines(Path p):Stream<String>;exists(Path p):boolean;createDirectories(Path p):Path;delete(Path p):void;list(Path dir):Stream<Path>;size(Path p):long"),
    ("Path", "of(String first, String... more):Path;resolve(String other):Path;getFileName():Path;getParent():Path;toAbsolutePath():Path;toFile():File"),
    ("Objects", "equals(Object a, Object b):boolean;hash(Object... values):int;requireNonNull(T obj):T;isNull(Object o):boolean;nonNull(Object o):boolean;toString(Object o):String"),
    ("Random", "nextInt(int bound):int;nextDouble():double;nextBoolean():boolean;nextLong():long;ints(long size):IntStream"),
    ("Iterator", "hasNext():boolean;next():E;remove():void"),
    ("Exception", "getMessage():String;printStackTrace():void;getCause():Throwable;getStackTrace():StackTraceElement[]"),
    ("array", "length:int;clone():T[]"),
];

/// More JDK members. An entry whose class already exists in `LIB` *adds* to it,
/// so these tables can grow without touching the originals.
const LIB_MORE: &[(&str, &str)] = &[
    ("Object", "notify():void;notifyAll():void;wait():void"),
    ("String", "codePointAt(int index):int;concat(String s):String;intern():String;getBytes():byte[];indent(int n):String;stripLeading():String;stripTrailing():String;stripIndent():String;compareToIgnoreCase(String s):int;contentEquals(CharSequence s):boolean;regionMatches(int off, String other, int ooff, int len):boolean;replaceFirst(String regex, String repl):String;indexOf(String str, int from):int;lastIndexOf(String str, int from):int;subSequence(int begin, int end):CharSequence;substring(int begin):String;split(String regex, int limit):String[];copyValueOf(char[] data):String;valueOf(char[] data):String;translateEscapes():String;describeConstable():Optional<String>;codePoints():IntStream;CASE_INSENSITIVE_ORDER:Comparator<String>"),
    ("StringBuilder", "capacity():int;ensureCapacity(int n):void;trimToSize():void;appendCodePoint(int cp):StringBuilder;substring(int start):String;substring(int start, int end):String;lastIndexOf(String s):int;codePointAt(int i):int;chars():IntStream;compareTo(StringBuilder other):int"),
    ("List", "containsAll(Collection c):boolean;lastIndexOf(Object o):int;retainAll(Collection c):boolean;removeAll(Collection c):boolean;replaceAll(UnaryOperator op):void;listIterator():ListIterator<E>;parallelStream():Stream<E>;add(int index, E e):void;addAll(int index, Collection c):boolean;equals(Object o):boolean;hashCode():int;toArray(T[] a):T[];ensureCapacity(int n):void;trimToSize():void"),
    ("Map", "replace(K key, V value):V;replaceAll(BiFunction f):void;remove(Object key, Object value):boolean;computeIfPresent(K key, BiFunction f):V;putAll(Map m):void;ofEntries(Map.Entry... entries):Map<K,V>;copyOf(Map m):Map<K,V>;firstKey():K;lastKey():K;floorKey(K key):K;ceilingKey(K key):K;higherKey(K key):K;lowerKey(K key):K;firstEntry():Map.Entry<K,V>;lastEntry():Map.Entry<K,V>;floorEntry(K key):Map.Entry<K,V>;ceilingEntry(K key):Map.Entry<K,V>;pollFirstEntry():Map.Entry<K,V>;pollLastEntry():Map.Entry<K,V>;headMap(K to):Map<K,V>;tailMap(K from):Map<K,V>;subMap(K from, K to):Map<K,V>;descendingMap():Map<K,V>;descendingKeySet():Set<K>;navigableKeySet():Set<K>;equals(Object o):boolean;hashCode():int"),
    ("Set", "containsAll(Collection c):boolean;toArray():Object[];parallelStream():Stream<E>;removeIf(Predicate filter):boolean;first():E;last():E;floor(E e):E;ceiling(E e):E;higher(E e):E;lower(E e):E;pollFirst():E;pollLast():E;headSet(E to):Set<E>;tailSet(E from):Set<E>;subSet(E from, E to):Set<E>;descendingSet():Set<E>;copyOf(Collection c):Set<E>;equals(Object o):boolean;hashCode():int"),
    ("Deque", "empty():boolean;search(Object o):int;offerFirst(E e):boolean;offerLast(E e):boolean;removeFirst():E;removeLast():E;getFirst():E;getLast():E;element():E;remove():E;add(E e):boolean;contains(Object o):boolean;iterator():Iterator<E>;descendingIterator():Iterator<E>;stream():Stream<E>;forEach(Consumer action):void;addAll(Collection c):boolean;removeFirstOccurrence(Object o):boolean;firstElement():E;lastElement():E"),
    ("Queue", "element():E;contains(Object o):boolean;iterator():Iterator<E>;stream():Stream<E>;forEach(Consumer action):void;addAll(Collection c):boolean;remove(Object o):boolean;toArray():Object[];comparator():Comparator<E>"),
    ("Entry", "getKey():K;getValue():V;setValue(V value):V;comparingByKey():Comparator;comparingByValue():Comparator"),
    ("Scanner", "nextFloat():float;nextShort():short;nextByte():byte;nextBigInteger():BigInteger;nextBigDecimal():BigDecimal;useDelimiter(String pattern):Scanner;skip(String pattern):Scanner;findInLine(String pattern):String;hasNextDouble():boolean;hasNextLong():boolean;hasNextBoolean():boolean"),
    ("Math", "abs(double a):double;max(double a, double b):double;min(double a, double b):double;max(long a, long b):long;min(long a, long b):long;signum(double d):double;rint(double a):double;atan(double a):double;atan2(double y, double x):double;asin(double a):double;acos(double a):double;sinh(double a):double;cosh(double a):double;tanh(double a):double;exp(double a):double;expm1(double a):double;log1p(double a):double;toDegrees(double rad):double;addExact(int x, int y):int;subtractExact(int x, int y):int;multiplyExact(int x, int y):int;negateExact(int a):int;toIntExact(long v):int;absExact(int a):int;ceilDiv(int x, int y):int;ceilMod(int x, int y):int;fma(double a, double b, double c):double;round(float a):int;floor(float a):float;TAU:double"),
    ("System", "setOut(PrintStream out):void;setErr(PrintStream err):void;setIn(InputStream in):void;gc():void;getProperties():Properties;setProperty(String key, String value):String;identityHashCode(Object x):int;getenv():Map<String,String>;console():Console;runFinalization():void"),
    ("PrintStream", "write(int b):void;append(CharSequence s):PrintStream;println(String x):void;println(int x):void;println(char x):void;println(double x):void;println(boolean x):void;println(long x):void;println(char[] x):void;println():void;print(String x):void;print(int x):void;print(char x):void;print(double x):void;close():void;checkError():boolean"),
    ("Integer", "parseInt(String s, int radix):int;valueOf(String s):Integer;toString():String;toString(int i, int radix):String;toOctalString(int i):String;bitCount(int i):int;signum(int i):int;reverse(int i):int;highestOneBit(int i):int;lowestOneBit(int i):int;numberOfLeadingZeros(int i):int;numberOfTrailingZeros(int i):int;parseUnsignedInt(String s):int;divideUnsigned(int a, int b):int;remainderUnsigned(int a, int b):int;compareTo(Integer other):int;longValue():long;doubleValue():double;floatValue():float;shortValue():short;byteValue():byte;equals(Object o):boolean;hashCode():int;hashCode(int i):int;decode(String s):Integer;rotateLeft(int i, int d):int;rotateRight(int i, int d):int;SIZE:int;BYTES:int"),
    ("Long", "parseLong(String s, int radix):long;valueOf(String s):Long;toString():String;sum(long a, long b):long;max(long a, long b):long;min(long a, long b):long;signum(long l):int;bitCount(long l):int;toBinaryString(long l):String;toHexString(long l):String;intValue():int;longValue():long;doubleValue():double;compareTo(Long other):int;equals(Object o):boolean;hashCode():int;numberOfTrailingZeros(long l):int;numberOfLeadingZeros(long l):int;SIZE:int;BYTES:int"),
    ("Double", "sum(double a, double b):double;max(double a, double b):double;min(double a, double b):double;isInfinite(double v):boolean;isFinite(double v):boolean;valueOf(String s):Double;toString():String;intValue():int;longValue():long;doubleValue():double;isNaN():boolean;compareTo(Double other):int;equals(Object o):boolean;hashCode():int;NaN:double;MIN_NORMAL:double"),
    ("Boolean", "booleanValue():boolean;toString(boolean b):String;logicalAnd(boolean a, boolean b):boolean;logicalOr(boolean a, boolean b):boolean;logicalXor(boolean a, boolean b):boolean;compare(boolean a, boolean b):int;equals(Object o):boolean"),
    ("Character", "isAlphabetic(int cp):boolean;isSpaceChar(char c):boolean;toString(char c):String;valueOf(char c):Character;charValue():char;digit(char c, int radix):int;forDigit(int digit, int radix):char;compare(char a, char b):int;isISOControl(char c):boolean;toChars(int cp):char[];getType(char c):int;MAX_VALUE:char;MIN_VALUE:char"),
    ("Arrays", "sort(Object[] a, int from, int to):void;sort(T[] a, Comparator c):void;parallelSort(Object[] a):void;fill(Object[] a, int from, int to, Object v):void;hashCode(Object[] a):int;deepEquals(Object[] a, Object[] b):boolean;setAll(Object[] a, IntFunction f):void;mismatch(Object[] a, Object[] b):int;compare(Object[] a, Object[] b):int;parallelPrefix(Object[] a, BinaryOperator op):void;stream(T[] a, int from, int to):Stream<T>;asList(T... a):List<T>"),
    ("Collections", "unmodifiableMap(Map m):Map<K,V>;unmodifiableSet(Set s):Set<T>;unmodifiableCollection(Collection c):Collection<T>;synchronizedList(List l):List<T>;synchronizedMap(Map m):Map<K,V>;emptyMap():Map<K,V>;emptySet():Set<T>;singletonList(T o):List<T>;singleton(T o):Set<T>;singletonMap(K k, V v):Map<K,V>;nCopies(int n, T o):List<T>;addAll(Collection c, T... items):boolean;binarySearch(List l, T key):int;reverseOrder():Comparator<T>;rotate(List l, int distance):void;fill(List l, T o):void;copy(List dest, List src):void;disjoint(Collection a, Collection b):boolean;sort(List l, Comparator c):void;max(Collection c, Comparator cmp):T;min(Collection c, Comparator cmp):T;shuffle(List l, Random rnd):void"),
    ("Stream", "mapToDouble(ToDoubleFunction f):DoubleStream;mapToLong(ToLongFunction f):LongStream;peek(Consumer action):Stream<T>;takeWhile(Predicate p):Stream<T>;dropWhile(Predicate p):Stream<T>;findAny():Optional<T>;toArray():Object[];iterator():Iterator<T>;parallel():Stream<T>;sequential():Stream<T>;unordered():Stream<T>;forEachOrdered(Consumer action):void;mapMulti(BiConsumer m):Stream<R>;sorted(Comparator c):Stream<T>;reduce(BinaryOperator op):Optional<T>;collect(Supplier s, BiConsumer a, BiConsumer c):R;concat(Stream a, Stream b):Stream<T>;iterate(T seed, UnaryOperator f):Stream<T>;generate(Supplier s):Stream<T>;empty():Stream<T>;ofNullable(T t):Stream<T>;close():void"),
    ("IntStream", "asLongStream():LongStream;asDoubleStream():DoubleStream;mapToObj(IntFunction f):Stream<R>;mapToLong(IntToLongFunction f):LongStream;mapToDouble(IntToDoubleFunction f):DoubleStream;flatMap(IntFunction f):IntStream;distinct():IntStream;sorted():IntStream;limit(long n):IntStream;skip(long n):IntStream;peek(IntConsumer a):IntStream;reduce(int identity, IntBinaryOperator op):int;anyMatch(IntPredicate p):boolean;allMatch(IntPredicate p):boolean;noneMatch(IntPredicate p):boolean;findFirst():OptionalInt;findAny():OptionalInt;summaryStatistics():IntSummaryStatistics;of(int... values):IntStream;iterate(int seed, IntUnaryOperator f):IntStream;generate(IntSupplier s):IntStream;concat(IntStream a, IntStream b):IntStream;empty():IntStream;parallel():IntStream;takeWhile(IntPredicate p):IntStream;dropWhile(IntPredicate p):IntStream"),
    ("Collectors", "toUnmodifiableList():Collector;toUnmodifiableSet():Collector;toCollection(Supplier s):Collector;joining():Collector;joining(CharSequence sep, CharSequence prefix, CharSequence suffix):Collector;groupingBy(Function f, Collector downstream):Collector;mapping(Function f, Collector downstream):Collector;summingInt(ToIntFunction f):Collector;summingLong(ToLongFunction f):Collector;summingDouble(ToDoubleFunction f):Collector;averagingInt(ToIntFunction f):Collector;averagingDouble(ToDoubleFunction f):Collector;minBy(Comparator c):Collector;maxBy(Comparator c):Collector;reducing(T identity, BinaryOperator op):Collector;collectingAndThen(Collector c, Function f):Collector;filtering(Predicate p, Collector c):Collector;flatMapping(Function f, Collector c):Collector;teeing(Collector a, Collector b, BiFunction f):Collector;summarizingInt(ToIntFunction f):Collector;partitioningBy(Predicate p, Collector c):Collector;toMap(Function k, Function v, BinaryOperator merge):Collector"),
    ("Optional", "filter(Predicate p):Optional<T>;flatMap(Function f):Optional<U>;or(Supplier s):Optional<T>;ifPresentOrElse(Consumer a, Runnable r):void;stream():Stream<T>;orElseThrow(Supplier s):T;equals(Object o):boolean;toString():String"),
    ("Thread", "setName(String name):void;getName():String;setDaemon(boolean on):void;isDaemon():boolean;getId():long;threadId():long;setPriority(int p):void;getPriority():int;isInterrupted():boolean;interrupted():boolean;join(long millis):void;yield():void;getState():Thread.State;isVirtual():boolean;setUncaughtExceptionHandler(Thread.UncaughtExceptionHandler h):void;onSpinWait():void;sleep(Duration d):void;MAX_PRIORITY:int;MIN_PRIORITY:int;NORM_PRIORITY:int"),
    ("Files", "newBufferedReader(Path p):BufferedReader;newBufferedWriter(Path p):BufferedWriter;write(Path p, byte[] data):Path;write(Path p, Iterable lines):Path;readAllBytes(Path p):byte[];isDirectory(Path p):boolean;isRegularFile(Path p):boolean;isReadable(Path p):boolean;isWritable(Path p):boolean;notExists(Path p):boolean;createFile(Path p):Path;createDirectory(Path p):Path;createTempFile(String prefix, String suffix):Path;createTempDirectory(String prefix):Path;deleteIfExists(Path p):boolean;copy(Path src, Path dst):Path;move(Path src, Path dst):Path;walk(Path start):Stream<Path>;find(Path start, int depth, BiPredicate m):Stream<Path>;getLastModifiedTime(Path p):FileTime;isSameFile(Path a, Path b):boolean"),
    ("Path", "getRoot():Path;getNameCount():int;getName(int i):Path;normalize():Path;relativize(Path other):Path;startsWith(String other):boolean;endsWith(String other):boolean;isAbsolute():boolean;resolveSibling(String other):Path;toString():String;toUri():URI;iterator():Iterator<Path>;compareTo(Path other):int;subpath(int from, int to):Path;resolve(Path other):Path;equals(Object o):boolean"),
    ("Objects", "requireNonNull(T obj, String message):T;requireNonNullElse(T obj, T def):T;requireNonNullElseGet(T obj, Supplier s):T;hashCode(Object o):int;compare(T a, T b, Comparator c):int;deepEquals(Object a, Object b):boolean;checkIndex(int index, int length):int;toIdentityString(Object o):String;toString(Object o, String nullDefault):String"),
    ("Random", "nextInt():int;nextInt(int origin, int bound):int;nextFloat():float;nextGaussian():double;nextBytes(byte[] bytes):void;nextLong(long bound):long;nextDouble(double origin, double bound):double;doubles(long size):DoubleStream;ints(long size, int origin, int bound):IntStream;setSeed(long seed):void"),
    ("Exception", "addSuppressed(Throwable t):void;getSuppressed():Throwable[];getLocalizedMessage():String;initCause(Throwable cause):Throwable;fillInStackTrace():Throwable;setStackTrace(StackTraceElement[] st):void;toString():String"),
    ("Iterator", "forEachRemaining(Consumer action):void;hasPrevious():boolean;previous():E;nextIndex():int;previousIndex():int;set(E e):void;add(E e):void"),
    ("Number", "intValue():int;longValue():long;doubleValue():double;floatValue():float;shortValue():short;byteValue():byte"),
    ("Comparable", "compareTo(T other):int"),
    ("Comparator", "compare(T a, T b):int;reversed():Comparator<T>;thenComparing(Comparator other):Comparator<T>;thenComparingInt(ToIntFunction f):Comparator<T>;thenComparingDouble(ToDoubleFunction f):Comparator<T>;thenComparingLong(ToLongFunction f):Comparator<T>;comparing(Function f):Comparator<T>;comparingInt(ToIntFunction f):Comparator<T>;comparingDouble(ToDoubleFunction f):Comparator<T>;comparingLong(ToLongFunction f):Comparator<T>;naturalOrder():Comparator<T>;reverseOrder():Comparator<T>;nullsFirst(Comparator c):Comparator<T>;nullsLast(Comparator c):Comparator<T>"),
    ("Runnable", "run():void"),
    ("Function", "apply(T t):R;andThen(Function after):Function<T,V>;compose(Function before):Function<V,R>;identity():Function<T,T>"),
    ("Supplier", "get():T"),
    ("Consumer", "accept(T t):void;andThen(Consumer after):Consumer<T>"),
    ("Predicate", "test(T t):boolean;and(Predicate other):Predicate<T>;or(Predicate other):Predicate<T>;negate():Predicate<T>;not(Predicate p):Predicate<T>;isEqual(Object o):Predicate<T>"),
    ("BiFunction", "apply(T t, U u):R;andThen(Function after):BiFunction<T,U,V>"),
    ("BiConsumer", "accept(T t, U u):void;andThen(BiConsumer after):BiConsumer<T,U>"),
    ("Enum", "name():String;ordinal():int;compareTo(E other):int;toString():String;equals(Object o):boolean;hashCode():int;getDeclaringClass():Class<E>;values():E[];valueOf(String name):E"),
    ("Record", "equals(Object o):boolean;hashCode():int;toString():String"),
    ("Class", "getName():String;getSimpleName():String;getPackageName():String;isInstance(Object o):boolean;isInterface():boolean;isEnum():boolean;isRecord():boolean;isArray():boolean;getSuperclass():Class<?>;getInterfaces():Class<?>[];getEnumConstants():Object[];getDeclaredMethods():Method[];getDeclaredFields():Field[];getMethods():Method[];getFields():Field[];newInstance():Object;cast(Object o):Object;forName(String name):Class<?>;getModifiers():int;getTypeName():String"),
    ("LocalDate", "now():LocalDate;of(int year, int month, int day):LocalDate;parse(CharSequence text):LocalDate;getYear():int;getMonthValue():int;getMonth():Month;getDayOfMonth():int;getDayOfWeek():DayOfWeek;getDayOfYear():int;plusDays(long n):LocalDate;plusWeeks(long n):LocalDate;plusMonths(long n):LocalDate;plusYears(long n):LocalDate;minusDays(long n):LocalDate;minusWeeks(long n):LocalDate;minusMonths(long n):LocalDate;minusYears(long n):LocalDate;isBefore(LocalDate other):boolean;isAfter(LocalDate other):boolean;isEqual(LocalDate other):boolean;isLeapYear():boolean;lengthOfMonth():int;withDayOfMonth(int d):LocalDate;withMonth(int m):LocalDate;withYear(int y):LocalDate;atStartOfDay():LocalDateTime;atTime(int h, int m):LocalDateTime;format(DateTimeFormatter f):String;toEpochDay():long;until(LocalDate end):Period;datesUntil(LocalDate end):Stream<LocalDate>;MIN:LocalDate;MAX:LocalDate;EPOCH:LocalDate"),
    ("LocalDateTime", "now():LocalDateTime;of(int y, int mo, int d, int h, int mi):LocalDateTime;parse(CharSequence text):LocalDateTime;getYear():int;getMonthValue():int;getDayOfMonth():int;getHour():int;getMinute():int;getSecond():int;getNano():int;getDayOfWeek():DayOfWeek;plusDays(long n):LocalDateTime;plusHours(long n):LocalDateTime;plusMinutes(long n):LocalDateTime;plusSeconds(long n):LocalDateTime;minusDays(long n):LocalDateTime;minusHours(long n):LocalDateTime;minusMinutes(long n):LocalDateTime;isBefore(LocalDateTime o):boolean;isAfter(LocalDateTime o):boolean;toLocalDate():LocalDate;toLocalTime():LocalTime;format(DateTimeFormatter f):String;withHour(int h):LocalDateTime;withMinute(int m):LocalDateTime;truncatedTo(TemporalUnit u):LocalDateTime"),
    ("LocalTime", "now():LocalTime;of(int h, int m):LocalTime;parse(CharSequence text):LocalTime;getHour():int;getMinute():int;getSecond():int;plusHours(long n):LocalTime;plusMinutes(long n):LocalTime;plusSeconds(long n):LocalTime;minusHours(long n):LocalTime;minusMinutes(long n):LocalTime;isBefore(LocalTime o):boolean;isAfter(LocalTime o):boolean;format(DateTimeFormatter f):String;toSecondOfDay():int;MIDNIGHT:LocalTime;NOON:LocalTime"),
    ("Duration", "ofSeconds(long s):Duration;ofMillis(long ms):Duration;ofMinutes(long m):Duration;ofHours(long h):Duration;ofDays(long d):Duration;between(Temporal a, Temporal b):Duration;toMillis():long;toSeconds():long;toMinutes():long;toHours():long;toDays():long;getSeconds():long;plus(Duration d):Duration;minus(Duration d):Duration;isNegative():boolean;isZero():boolean;ZERO:Duration"),
    ("Instant", "now():Instant;ofEpochMilli(long ms):Instant;ofEpochSecond(long s):Instant;toEpochMilli():long;getEpochSecond():long;plusSeconds(long n):Instant;plusMillis(long n):Instant;minusSeconds(long n):Instant;isBefore(Instant o):boolean;isAfter(Instant o):boolean;parse(CharSequence text):Instant"),
    ("StringJoiner", "add(CharSequence s):StringJoiner;toString():String;length():int;setEmptyValue(CharSequence s):StringJoiner"),
    ("BigInteger", "add(BigInteger v):BigInteger;subtract(BigInteger v):BigInteger;multiply(BigInteger v):BigInteger;divide(BigInteger v):BigInteger;mod(BigInteger m):BigInteger;remainder(BigInteger v):BigInteger;pow(int e):BigInteger;modPow(BigInteger e, BigInteger m):BigInteger;gcd(BigInteger v):BigInteger;abs():BigInteger;negate():BigInteger;signum():int;compareTo(BigInteger v):int;equals(Object o):boolean;intValue():int;longValue():long;toString(int radix):String;valueOf(long v):BigInteger;isProbablePrime(int c):boolean;sqrt():BigInteger;max(BigInteger v):BigInteger;min(BigInteger v):BigInteger;bitLength():int;ZERO:BigInteger;ONE:BigInteger;TWO:BigInteger;TEN:BigInteger"),
    ("BigDecimal", "add(BigDecimal v):BigDecimal;subtract(BigDecimal v):BigDecimal;multiply(BigDecimal v):BigDecimal;divide(BigDecimal v):BigDecimal;divide(BigDecimal v, int scale, RoundingMode m):BigDecimal;setScale(int scale, RoundingMode m):BigDecimal;pow(int n):BigDecimal;abs():BigDecimal;negate():BigDecimal;signum():int;compareTo(BigDecimal v):int;equals(Object o):boolean;doubleValue():double;intValue():int;longValue():long;toPlainString():String;stripTrailingZeros():BigDecimal;valueOf(double v):BigDecimal;scale():int;precision():int;max(BigDecimal v):BigDecimal;min(BigDecimal v):BigDecimal;ZERO:BigDecimal;ONE:BigDecimal;TEN:BigDecimal"),
    ("AtomicInteger", "get():int;set(int v):void;incrementAndGet():int;decrementAndGet():int;getAndIncrement():int;getAndDecrement():int;addAndGet(int d):int;getAndAdd(int d):int;compareAndSet(int expect, int update):boolean;updateAndGet(IntUnaryOperator f):int;getAndUpdate(IntUnaryOperator f):int;accumulateAndGet(int x, IntBinaryOperator f):int;intValue():int"),
    ("AtomicLong", "get():long;set(long v):void;incrementAndGet():long;decrementAndGet():long;getAndIncrement():long;getAndDecrement():long;addAndGet(long d):long;getAndAdd(long d):long;compareAndSet(long expect, long update):boolean;updateAndGet(LongUnaryOperator f):long;longValue():long"),
    ("ExecutorService", "submit(Callable task):Future<T>;execute(Runnable task):void;shutdown():void;shutdownNow():List<Runnable>;awaitTermination(long timeout, TimeUnit unit):boolean;isShutdown():boolean;isTerminated():boolean;invokeAll(Collection tasks):List<Future<T>>;invokeAny(Collection tasks):T;close():void"),
    ("Executors", "newFixedThreadPool(int n):ExecutorService;newCachedThreadPool():ExecutorService;newSingleThreadExecutor():ExecutorService;newVirtualThreadPerTaskExecutor():ExecutorService;newWorkStealingPool():ExecutorService;newScheduledThreadPool(int n):ScheduledExecutorService;newSingleThreadScheduledExecutor():ScheduledExecutorService"),
    ("Future", "get():T;get(long timeout, TimeUnit unit):T;isDone():boolean;isCancelled():boolean;cancel(boolean mayInterrupt):boolean;resultNow():T"),
    ("CompletableFuture", "supplyAsync(Supplier s):CompletableFuture<T>;runAsync(Runnable r):CompletableFuture<Void>;thenApply(Function f):CompletableFuture<U>;thenAccept(Consumer c):CompletableFuture<Void>;thenRun(Runnable r):CompletableFuture<Void>;thenCompose(Function f):CompletableFuture<U>;thenCombine(CompletionStage other, BiFunction f):CompletableFuture<V>;exceptionally(Function f):CompletableFuture<T>;handle(BiFunction f):CompletableFuture<U>;whenComplete(BiConsumer a):CompletableFuture<T>;join():T;get():T;complete(T value):boolean;completeExceptionally(Throwable ex):boolean;isDone():boolean;allOf(CompletableFuture... cfs):CompletableFuture<Void>;anyOf(CompletableFuture... cfs):CompletableFuture<Object>;completedFuture(U value):CompletableFuture<U>;orTimeout(long t, TimeUnit u):CompletableFuture<T>"),
    ("TimeUnit", "sleep(long timeout):void;toMillis(long d):long;toSeconds(long d):long;toNanos(long d):long;toMinutes(long d):long;convert(long d, TimeUnit u):long;NANOSECONDS:TimeUnit;MICROSECONDS:TimeUnit;MILLISECONDS:TimeUnit;SECONDS:TimeUnit;MINUTES:TimeUnit;HOURS:TimeUnit;DAYS:TimeUnit"),
    ("Pattern", "compile(String regex):Pattern;compile(String regex, int flags):Pattern;matcher(CharSequence input):Matcher;matches(String regex, CharSequence input):boolean;split(CharSequence input):String[];pattern():String;quote(String s):String;asPredicate():Predicate<String>;splitAsStream(CharSequence in):Stream<String>;CASE_INSENSITIVE:int;MULTILINE:int;DOTALL:int"),
    ("Matcher", "matches():boolean;find():boolean;find(int start):boolean;lookingAt():boolean;group():String;group(int g):String;group(String name):String;groupCount():int;start():int;start(int g):int;end():int;end(int g):int;replaceAll(String repl):String;replaceFirst(String repl):String;reset():Matcher;hitEnd():boolean;results():Stream<MatchResult>"),
    ("UUID", "randomUUID():UUID;fromString(String s):UUID;toString():String;nameUUIDFromBytes(byte[] b):UUID;getMostSignificantBits():long;getLeastSignificantBits():long;version():int"),
    ("BufferedReader", "readLine():String;read():int;ready():boolean;lines():Stream<String>;close():void;skip(long n):long;mark(int limit):void;reset():void"),
    ("InputStream", "read():int;read(byte[] b):int;readAllBytes():byte[];readNBytes(int n):byte[];available():int;close():void;skip(long n):long;transferTo(OutputStream out):long"),
    ("File", "exists():boolean;isFile():boolean;isDirectory():boolean;getName():String;getPath():String;getAbsolutePath():String;getParent():String;getParentFile():File;length():long;delete():boolean;mkdir():boolean;mkdirs():boolean;createNewFile():boolean;listFiles():File[];list():String[];canRead():boolean;canWrite():boolean;lastModified():long;renameTo(File dest):boolean;toPath():Path;separator:String;pathSeparator:String"),
    ("Paths", "get(String first, String... more):Path"),
    ("PrintWriter", "println(Object x):void;print(Object x):void;printf(String format, Object... args):PrintWriter;write(String s):void;flush():void;close():void;append(CharSequence s):PrintWriter"),
    ("FileWriter", "write(String s):void;append(CharSequence s):FileWriter;flush():void;close():void"),
    ("FileReader", "read():int;close():void;ready():boolean"),
    ("ThreadLocalRandom", "current():ThreadLocalRandom;nextInt(int bound):int;nextInt(int origin, int bound):int;nextDouble():double;nextBoolean():boolean;nextLong():long"),
    ("ConcurrentHashMap", "newKeySet():Set<K>;mappingCount():long;reduceValues(long p, BiFunction f):V"),
    ("ReentrantLock", "lock():void;unlock():void;tryLock():boolean;lockInterruptibly():void;isLocked():boolean;isHeldByCurrentThread():boolean;newCondition():Condition;getHoldCount():int"),
    ("Byte", "parseByte(String s):byte;valueOf(byte b):Byte;toString(byte b):String;MAX_VALUE:byte;MIN_VALUE:byte"),
    ("Short", "parseShort(String s):short;valueOf(short s):Short;toString(short s):String;MAX_VALUE:short;MIN_VALUE:short"),
    ("Float", "parseFloat(String s):float;valueOf(float f):Float;toString(float f):String;compare(float a, float b):int;isNaN(float f):boolean;MAX_VALUE:float;MIN_VALUE:float"),
    ("CharSequence", "length():int;charAt(int index):char;subSequence(int a, int b):CharSequence;toString():String;isEmpty():boolean;chars():IntStream"),
    ("Iterable", "iterator():Iterator<E>;forEach(Consumer action):void;spliterator():Spliterator<E>"),
    ("DoubleStream", "sum():double;average():OptionalDouble;max():OptionalDouble;min():OptionalDouble;count():long;boxed():Stream<Double>;map(DoubleUnaryOperator f):DoubleStream;filter(DoublePredicate p):DoubleStream;forEach(DoubleConsumer a):void;toArray():double[];of(double... values):DoubleStream;mapToObj(DoubleFunction f):Stream<R>;summaryStatistics():DoubleSummaryStatistics"),
    ("LongStream", "range(long a, long b):LongStream;rangeClosed(long a, long b):LongStream;sum():long;average():OptionalDouble;max():OptionalLong;min():OptionalLong;count():long;boxed():Stream<Long>;map(LongUnaryOperator f):LongStream;filter(LongPredicate p):LongStream;forEach(LongConsumer a):void;toArray():long[];of(long... values):LongStream;mapToObj(LongFunction f):Stream<R>"),
    ("OptionalInt", "isPresent():boolean;isEmpty():boolean;getAsInt():int;orElse(int other):int;orElseThrow():int;ifPresent(IntConsumer a):void"),
    ("OptionalDouble", "isPresent():boolean;isEmpty():boolean;getAsDouble():double;orElse(double other):double;orElseThrow():double;ifPresent(DoubleConsumer a):void"),
    ("OptionalLong", "isPresent():boolean;isEmpty():boolean;getAsLong():long;orElse(long other):long;orElseThrow():long;ifPresent(LongConsumer a):void"),
    ("IntSummaryStatistics", "getMin():int;getMax():int;getAverage():double;getSum():long;getCount():long;accept(int v):void"),
    ("DoubleSummaryStatistics", "getMin():double;getMax():double;getAverage():double;getSum():double;getCount():long;accept(double v):void"),
    ("Properties", "getProperty(String key):String;getProperty(String key, String def):String;setProperty(String key, String value):Object;load(InputStream in):void;store(OutputStream out, String comments):void;stringPropertyNames():Set<String>"),
];

/// Implementation classes that share an interface's member table.
const ALIASES: &[(&str, &str)] = &[
    ("ArrayList", "List"), ("LinkedList", "List"), ("Collection", "List"), ("Iterable", "List"),
    ("HashMap", "Map"), ("TreeMap", "Map"), ("LinkedHashMap", "Map"),
    ("HashSet", "Set"), ("TreeSet", "Set"), ("LinkedHashSet", "Set"),
    ("ArrayDeque", "Deque"), ("Stack", "Deque"), ("PriorityQueue", "Queue"),
    ("RuntimeException", "Exception"), ("Throwable", "Exception"), ("IOException", "Exception"),
    ("IllegalArgumentException", "Exception"), ("IllegalStateException", "Exception"),
    ("CharSequence", "String"),
    ("Vector", "List"), ("CopyOnWriteArrayList", "List"), ("AbstractList", "List"),
    ("EnumMap", "Map"), ("ConcurrentHashMap", "Map"), ("Hashtable", "Map"), ("IdentityHashMap", "Map"),
    ("WeakHashMap", "Map"), ("NavigableMap", "Map"), ("SortedMap", "Map"), ("ConcurrentMap", "Map"),
    ("EnumSet", "Set"), ("NavigableSet", "Set"), ("SortedSet", "Set"), ("ConcurrentSkipListSet", "Set"),
    ("ConcurrentLinkedQueue", "Queue"), ("LinkedBlockingQueue", "Queue"), ("BlockingQueue", "Queue"),
    ("ArrayBlockingQueue", "Queue"), ("PriorityBlockingQueue", "Queue"),
    ("ConcurrentLinkedDeque", "Deque"), ("LinkedBlockingDeque", "Deque"),
    ("Error", "Exception"), ("InterruptedException", "Exception"), ("NumberFormatException", "Exception"),
    ("NullPointerException", "Exception"), ("ArithmeticException", "Exception"),
    ("IndexOutOfBoundsException", "Exception"), ("ArrayIndexOutOfBoundsException", "Exception"),
    ("UnsupportedOperationException", "Exception"), ("FileNotFoundException", "Exception"),
    ("TimeoutException", "Exception"), ("ExecutionException", "Exception"),
    ("ClassCastException", "Exception"), ("CloneNotSupportedException", "Exception"),
    ("StackOverflowError", "Exception"), ("OutOfMemoryError", "Exception"), ("AssertionError", "Exception"),
    ("UncheckedIOException", "Exception"), ("NoSuchElementException", "Exception"),
    ("ConcurrentModificationException", "Exception"), ("DateTimeException", "Exception"),
    ("ScheduledExecutorService", "ExecutorService"), ("StringBuffer", "StringBuilder"),
    ("ListIterator", "Iterator"), ("Spliterator", "Iterator"),
    ("Map.Entry", "Entry"), ("AbstractMap.SimpleEntry", "Entry"), ("SimpleEntry", "Entry"),
    ("BufferedWriter", "PrintWriter"), ("Writer", "PrintWriter"), ("OutputStreamWriter", "PrintWriter"),
    ("Reader", "BufferedReader"), ("InputStreamReader", "BufferedReader"), ("FileInputStream", "InputStream"),
    ("ByteArrayInputStream", "InputStream"), ("BufferedInputStream", "InputStream"),
    ("Callable", "Supplier"), ("UnaryOperator", "Function"), ("BinaryOperator", "BiFunction"),
    ("IntFunction", "Function"), ("ToIntFunction", "Function"), ("BiPredicate", "Predicate"),
    ("IntPredicate", "Predicate"), ("IntConsumer", "Consumer"), ("BooleanSupplier", "Supplier"),
    ("IntSupplier", "Supplier"), ("DoubleSupplier", "Supplier"), ("LongSupplier", "Supplier"),
    ("ZonedDateTime", "LocalDateTime"), ("OffsetDateTime", "LocalDateTime"),
];

const EXTRA_CLASSES: &[&str] = &[
    "ArrayList", "LinkedList", "HashMap", "TreeMap", "LinkedHashMap", "HashSet", "TreeSet",
    "LinkedHashSet", "ArrayDeque", "Stack", "PriorityQueue", "Collection", "Iterable",
    "RuntimeException", "Throwable", "IOException", "IllegalArgumentException",
    "IllegalStateException", "NullPointerException", "IndexOutOfBoundsException",
    "UnsupportedOperationException", "Comparable", "Comparator", "Runnable", "Function",
    "Supplier", "Consumer", "Predicate", "BiFunction", "BiConsumer", "LocalDate", "LocalDateTime",
    "LocalTime", "Duration", "Instant", "Record", "Enum", "Byte", "Short", "Float", "Number",
    "CharSequence", "Paths", "File", "BigInteger", "BigDecimal", "AtomicInteger", "AtomicLong",
    "ExecutorService", "Executors", "CompletableFuture", "Pattern", "Matcher", "UUID",
    "BufferedReader", "InputStreamReader", "FileReader", "FileWriter", "PrintWriter",
    "Override", "FunctionalInterface", "SuppressWarnings", "Deprecated", "StringJoiner",
    "ConcurrentHashMap", "ReentrantLock", "StructuredTaskScope", "ScopedValue",
    "Vector", "CopyOnWriteArrayList", "EnumMap", "EnumSet", "Hashtable", "NavigableMap", "SortedMap",
    "NavigableSet", "SortedSet", "BlockingQueue", "LinkedBlockingQueue", "ArrayBlockingQueue",
    "ConcurrentLinkedQueue", "Error", "InterruptedException", "NumberFormatException",
    "ArithmeticException", "ArrayIndexOutOfBoundsException", "FileNotFoundException",
    "TimeoutException", "ExecutionException", "ClassCastException", "NoSuchElementException",
    "AssertionError", "StackOverflowError", "OutOfMemoryError", "UncheckedIOException",
    "ConcurrentModificationException", "DateTimeException", "StringBuffer", "ListIterator",
    "Callable", "UnaryOperator", "BinaryOperator", "BiPredicate", "IntPredicate", "IntFunction",
    "ToIntFunction", "IntConsumer", "IntSupplier", "BooleanSupplier", "DoubleSupplier",
    "ZonedDateTime", "ZoneId", "DateTimeFormatter", "ChronoUnit", "DayOfWeek", "Month", "Period",
    "Year", "YearMonth", "Writer", "Reader", "BufferedWriter", "OutputStream", "FileInputStream",
    "FileOutputStream", "ByteArrayInputStream", "ByteArrayOutputStream", "Charset",
    "StandardCharsets", "Files", "Collector", "Spliterator", "RoundingMode", "MathContext",
    "Void", "Process", "ProcessBuilder", "Runtime", "StackTraceElement", "Iterable", "Cloneable",
    "AutoCloseable", "Closeable", "Serializable", "Annotation", "Retention", "Target",
    "SafeVarargs", "Objects", "Optional", "OptionalInt", "OptionalDouble", "OptionalLong",
    "DoubleStream", "LongStream", "IntSummaryStatistics", "DoubleSummaryStatistics", "Properties",
    "ThreadLocalRandom", "TimeUnit", "Future", "ScheduledExecutorService", "CountDownLatch",
    "Semaphore", "CyclicBarrier", "ReentrantReadWriteLock", "Condition", "Lock", "AtomicBoolean",
    "AtomicReference", "LongAdder", "Thread", "ThreadLocal", "Timer", "TimerTask", "Locale",
    "Currency", "Base64", "BitSet", "Calendar", "Date", "TreeMap", "Scanner", "Console",
    "SecureRandom", "MessageDigest", "URI", "URL", "HttpClient", "HttpRequest", "HttpResponse",
    "Stream", "IntStream", "Collectors", "Arrays", "Collections", "Math", "StrictMath", "System",
    "Integer", "Long", "Double", "Boolean", "Character", "String", "StringBuilder", "Object",
    "Entry", "SimpleEntry", "ArrayList", "List", "Map", "Set", "Deque", "Queue", "Random",
    "Iterator", "Exception", "Path", "File", "Record", "Enum", "Class", "Number", "Comparable",
    "Comparator", "Runnable", "Function", "Supplier", "Consumer", "Predicate", "BiFunction",
    "BiConsumer", "LocalDate", "LocalDateTime", "LocalTime", "Duration", "Instant", "UUID",
    "Pattern", "Matcher", "BufferedReader", "InputStream", "PrintStream", "PrintWriter",
    "FileWriter", "FileReader", "BigInteger", "BigDecimal", "AtomicInteger", "AtomicLong",
    "ExecutorService", "Executors", "CompletableFuture", "StringJoiner", "Paths", "Byte", "Short",
    "Float", "CharSequence", "Override", "FunctionalInterface", "SuppressWarnings", "Deprecated",
];

const SNIPPETS: &[(&str, &str, &str)] = &[
    ("sout", "System.out.println($0);", "print line"),
    ("souf", "System.out.printf(\"$0%n\");", "printf"),
    ("serr", "System.err.println($0);", "print to stderr"),
    ("println", "IO.println($0);", "Java 25 IO.println"),
    ("readln", "IO.readln(\"$0\")", "Java 25 IO.readln"),
    ("psvm", "public static void main(String[] args) {\n    $0\n}", "main method"),
    ("main", "void main() {\n    $0\n}", "Java 25 instance main"),
    ("fori", "for (int i = 0; i < $0; i++) {\n    \n}", "indexed for loop"),
    ("foreach", "for (var item : $0) {\n    \n}", "for-each loop"),
    ("iter", "for (var item : $0) {\n    \n}", "for-each loop"),
    ("whilel", "while ($0) {\n    \n}", "while loop"),
    ("ifs", "if ($0) {\n    \n}", "if statement"),
    ("ifelse", "if ($0) {\n    \n} else {\n    \n}", "if / else"),
    ("trycatch", "try {\n    $0\n} catch (Exception e) {\n    e.printStackTrace();\n}", "try / catch"),
    ("switchx", "switch ($0) {\n    case  -> {}\n    default -> {}\n}", "switch expression"),
    ("classd", "public class $0 {\n    \n}", "class declaration"),
    ("recordd", "public record $0() {}", "record declaration"),
    ("interfaced", "public interface $0 {\n    \n}", "interface declaration"),
    ("enumd", "public enum $0 {\n    \n}", "enum declaration"),
    ("scanner", "Scanner scanner = new Scanner(System.in);$0", "stdin scanner"),
    ("lambda", "($0) -> {}", "lambda"),
];

fn lib_members(class: &str) -> Option<Vec<Item>> {
    thread_local! {
        static CACHE: RefCell<HashMap<String, Option<Rc<Vec<Item>>>>> = RefCell::new(HashMap::new());
    }
    let cached = CACHE.with(|c| c.borrow().get(class).cloned());
    let rc = match cached {
        Some(v) => v,
        None => {
            let built = build_lib_members(class).map(Rc::new);
            CACHE.with(|c| c.borrow_mut().insert(class.to_string(), built.clone()));
            built
        }
    };
    rc.map(|r| (*r).clone())
}

fn build_lib_members(class: &str) -> Option<Vec<Item>> {
    let canonical = ALIASES.iter().find(|(a, _)| *a == class).map(|(_, c)| *c).unwrap_or(class);
    let mut any = false;
    let mut out = Vec::new();
    // the canonical table first, then the class's own table (if it is an alias with extras)
    for name in [canonical, class] {
        if name == class && canonical == class && any {
            break;
        }
        for (n, spec) in LIB.iter().chain(LIB_MORE.iter()) {
            if *n != name {
                continue;
            }
            any = true;
            for m in spec.split(';') {
                let Some((sig, ret)) = m.rsplit_once(':') else { continue };
                out.push(match sig.find('(') {
                    Some(p) => Item::method(&sig[..p], sig[p + 1..].trim_end_matches(')'), ret),
                    None => Item::new(sig, sig, ret, Kind::Field),
                });
            }
        }
    }
    any.then_some(out)
}

fn all_lib_class_names() -> impl Iterator<Item = &'static str> {
    LIB.iter()
        .chain(LIB_MORE.iter())
        .map(|(n, _)| *n)
        .filter(|n| *n != "array" && !n.contains('.'))
        .chain(ALIASES.iter().map(|(a, _)| *a).filter(|n| !n.contains('.')))
        .chain(EXTRA_CLASSES.iter().copied())
}

// ------------------------------------------------------------ project index

#[derive(Clone, Default)]
pub struct ClassInfo {
    pub name: String,
    pub file: PathBuf,
    pub members: Vec<Item>,
    /// Names of the classes / interfaces this one extends or implements.
    pub supers: Vec<String>,
}

#[derive(Default)]
pub struct ProjectIndex {
    pub classes: HashMap<String, ClassInfo>,
}

impl ProjectIndex {
    pub fn rebuild(&mut self, root: &Path) {
        self.classes.clear();
        let mut files = Vec::new();
        crate::java::collect_sources(root, &mut files);
        for f in files.into_iter().take(3000) {
            if let Ok(src) = std::fs::read_to_string(&f) {
                self.update_file(&f, &src);
            }
        }
    }

    pub fn update_file(&mut self, path: &Path, src: &str) {
        self.classes.retain(|_, c| c.file != path);
        for mut c in parse_classes(src) {
            c.file = path.to_path_buf();
            self.classes.insert(c.name.clone(), c);
        }
    }
}

/// Replace comments and string/char literals with spaces, preserving offsets.
/// The last result is cached, because completion asks for it several times per keystroke.
pub fn blank_comments_and_strings(src: &str) -> String {
    thread_local! {
        static LAST: RefCell<Option<(u64, usize, Rc<String>)>> = const { RefCell::new(None) };
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    let key = (h.finish(), src.len());
    if let Some(hit) = LAST.with(|l| l.borrow().as_ref().filter(|(k, n, _)| (*k, *n) == key).map(|(_, _, s)| s.clone())) {
        return (*hit).clone();
    }
    let out = blank_uncached(src);
    LAST.with(|l| *l.borrow_mut() = Some((key.0, key.1, Rc::new(out.clone()))));
    out
}

fn blank_uncached(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let n = b.len();
    let mut i = 0;
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for x in out.iter_mut().take(to.min(n)).skip(from) {
            if *x != b'\n' {
                *x = b' ';
            }
        }
    };
    while i < n {
        let s = i;
        match b[i] {
            b'/' if i + 1 < n && b[i + 1] == b'/' => {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
                blank(&mut out, s, i);
            }
            b'/' if i + 1 < n && b[i + 1] == b'*' => {
                i += 2;
                while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(n);
                blank(&mut out, s, i);
            }
            q @ (b'"' | b'\'') => {
                let triple = q == b'"' && src[i..].starts_with("\"\"\"");
                i += if triple { 3 } else { 1 };
                while i < n {
                    if b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if triple {
                        if src.as_bytes()[i..].starts_with(b"\"\"\"") {
                            i += 3;
                            break;
                        }
                    } else if b[i] == q || b[i] == b'\n' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                // keep the quotes so `"x".` can still be recognised as a String
                blank(&mut out, s + 1, i.saturating_sub(1).max(s + 1));
            }
            _ => i += 1,
        }
    }
    // Only ASCII bytes were replaced by ASCII spaces, but a blanked range may
    // have cut through multi-byte chars; lossy conversion keeps offsets for ASCII.
    String::from_utf8(out).unwrap_or_else(|e| {
        e.into_bytes().iter().map(|&c| if c < 0x80 { c as char } else { ' ' }).collect()
    })
}

static CLASS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(class|interface|enum|record)\s+([A-Za-z_$][\w$]*)").unwrap());
static RECORD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\brecord\s+([A-Za-z_$][\w$]*)\s*(?:<[^>]*>)?\s*\(([^)]*)\)").unwrap()
});
static METHOD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)(?:^|[{};])[ \t]*((?:(?:public|private|protected|static|final|abstract|synchronized|default|native|strictfp)\s+)*)(?:<[^>]*>\s*)?([\w$.]+(?:<[^;(){}]*>)?(?:\[\])*)\s+([\w$]+)\s*\(([^)]*)\)").unwrap()
});
static FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)(?:^|[{};])[ \t]*((?:(?:public|private|protected|static|final|volatile|transient)\s+)*)([\w$.]+(?:<[^;(){}=]*>)?(?:\[\])*)\s+([\w$]+)\s*(?:=|;)").unwrap()
});
static IDENT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z_$][\w$]*").unwrap());

const NOT_TYPES: &[&str] = &[
    "return", "new", "else", "throw", "case", "yield", "package", "import", "public", "private",
    "protected", "static", "final", "abstract", "synchronized", "default", "native", "strictfp",
    "volatile", "transient", "goto", "assert",
];
const NOT_NAMES: &[&str] = &["if", "for", "while", "switch", "catch", "synchronized", "return", "new", "try"];

struct ClassSpan {
    body_start: usize,
    body_end: usize,
    body_depth: i32,
}

pub fn parse_classes(src: &str) -> Vec<ClassInfo> {
    let clean = blank_comments_and_strings(src);
    let bytes = clean.as_bytes();
    // brace depth *before* each byte
    let mut depth = vec![0i32; bytes.len() + 1];
    let mut d = 0;
    for (i, &c) in bytes.iter().enumerate() {
        depth[i] = d;
        if c == b'{' {
            d += 1;
        } else if c == b'}' {
            d -= 1;
        }
    }
    depth[bytes.len()] = d;

    let mut spans: Vec<ClassSpan> = Vec::new();
    let mut infos: Vec<ClassInfo> = Vec::new();
    for c in CLASS_RE.captures_iter(&clean) {
        let m = c.get(0).unwrap();
        let name = c[2].to_string();
        let Some(open_rel) = clean[m.end()..].find('{') else { continue };
        let open = m.end() + open_rel;
        let body_depth = depth[open] + 1;
        let mut end = bytes.len();
        for (i, &ch) in bytes.iter().enumerate().skip(open + 1) {
            if ch == b'}' && depth[i] == body_depth {
                end = i;
                break;
            }
        }
        spans.push(ClassSpan { body_start: open + 1, body_end: end, body_depth });
        let mut info = ClassInfo { name, supers: parse_supers(&clean[m.end()..open]), ..Default::default() };
        match &c[1] {
            "enum" => {
                enum_constants(&clean[open + 1..end], &info.name, &mut info.members);
                info.supers.push("Enum".into());
            }
            "record" => info.supers.push("Record".into()),
            _ => {}
        }
        infos.push(info);
    }
    let owner = |pos: usize| -> Option<usize> {
        spans
            .iter()
            .enumerate()
            .filter(|(_, s)| pos >= s.body_start && pos < s.body_end && depth[pos] == s.body_depth)
            .map(|(i, _)| i)
            .next_back()
    };

    for c in RECORD_RE.captures_iter(&clean) {
        if let Some(info) = infos.iter_mut().find(|i| i.name == c[1]) {
            for comp in c[2].split(',') {
                let mut parts = comp.split_whitespace().rev();
                if let (Some(n), Some(t)) = (parts.next(), parts.next()) {
                    info.members.push(Item::method(n, "", t));
                }
            }
        }
    }
    for c in METHOD_RE.captures_iter(&clean) {
        let (ty, name) = (&c[2], &c[3]);
        if NOT_TYPES.contains(&ty) || NOT_NAMES.contains(&name) {
            continue;
        }
        let pos = c.get(3).unwrap().start();
        if let Some(o) = owner(pos) {
            let params = c[4].split_whitespace().collect::<Vec<_>>().join(" ");
            infos[o].members.push(Item::method(name, &params, ty));
        }
    }
    for c in FIELD_RE.captures_iter(&clean) {
        let (ty, name) = (&c[2], &c[3]);
        if NOT_TYPES.contains(&ty) || KEYWORDS.contains(&name) {
            continue;
        }
        let pos = c.get(3).unwrap().start();
        if let Some(o) = owner(pos) {
            infos[o].members.push(Item::new(name, name, ty, Kind::Field));
        }
    }
    for i in &mut infos {
        let mut seen = HashSet::new();
        i.members.retain(|m| seen.insert((m.label.clone(), m.kind)));
    }
    infos
}

/// `extends A<B>, C implements D` → `["A", "C", "D"]` (generic arguments dropped).
fn parse_supers(header: &str) -> Vec<String> {
    let mut flat = String::new();
    let (mut angle, mut paren) = (0i32, 0i32);
    for ch in header.chars() {
        match ch {
            '<' => angle += 1,
            '>' => angle = (angle - 1).max(0),
            '(' => paren += 1,
            ')' => paren = (paren - 1).max(0),
            _ if angle == 0 && paren == 0 => flat.push(ch),
            _ => {}
        }
    }
    let mut out = Vec::new();
    let mut on = false;
    for tok in flat.split(|c: char| c.is_whitespace() || c == ',') {
        match tok {
            "" => {}
            "extends" | "implements" => on = true,
            "permits" => on = false,
            t if on => out.push(t.rsplit('.').next().unwrap_or(t).to_string()),
            _ => {}
        }
    }
    out
}

/// The constants at the top of an enum body (`A, B(1), C { ... };`) as fields.
fn enum_constants(body: &str, enum_name: &str, members: &mut Vec<Item>) {
    let mut depth = 0i32;
    let mut end = body.len();
    for (i, ch) in body.char_indices() {
        match ch {
            '(' | '{' => depth += 1,
            ')' | '}' => depth -= 1,
            ';' if depth == 0 => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    let region = &body[..end];
    let mut depth = 0i32;
    let mut part_start = 0;
    let mut parts: Vec<&str> = Vec::new();
    for (i, ch) in region.char_indices() {
        match ch {
            '(' | '{' => depth += 1,
            ')' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&region[part_start..i]);
                part_start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&region[part_start..]);
    for p in parts {
        let p = p.trim();
        if let Some(m) = IDENT_RE.find(p).filter(|m| m.start() == 0) {
            members.push(Item::new(m.as_str(), m.as_str(), enum_name, Kind::Field));
        }
    }
}

// --------------------------------------------------------------- completion

pub struct Completions {
    /// Byte offset where the word being completed starts.
    pub start: usize,
    pub items: Vec<Item>,
}

pub fn in_string_or_comment(before: &str) -> bool {
    if let Some(o) = before.rfind("/*") {
        if !before[o..].contains("*/") {
            return true;
        }
    }
    let line = &before[before.rfind('\n').map_or(0, |i| i + 1)..];
    let mut in_str = false;
    let mut prev = '\0';
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if !in_str && c == '/' && chars.peek() == Some(&'/') {
            return true;
        }
        if c == '"' && prev != '\\' {
            in_str = !in_str;
        }
        prev = if prev == '\\' && c == '\\' { '\0' } else { c };
    }
    in_str
}

pub fn complete(
    text: &str,
    cursor: usize,
    index: &ProjectIndex,
    manual: bool,
    recent: &[String],
) -> Option<Completions> {
    let cursor = cursor.min(text.len());
    let before = &text[..cursor];
    if in_string_or_comment(before) {
        return None;
    }
    let b = text.as_bytes();
    let mut start = cursor;
    while start > 0 && is_ident(b[start - 1]) {
        start -= 1;
    }
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let prefix = &text[start..cursor];
    if prefix.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return None;
    }

    // `receiver.prefix` (the dot may sit on the previous line in fluent chains)
    let mut dot = start;
    while dot > 0 && b[dot - 1].is_ascii_whitespace() {
        dot -= 1;
    }
    let mut items: Vec<Item>;
    if dot > 0 && b[dot - 1] == b'.' {
        let clean = blank_comments_and_strings(text);
        let receiver = receiver_before(&clean, dot - 1);
        items = member_items(&receiver, text, cursor, index);
    } else {
        // A single typed letter is enough to open the popup.
        if prefix.is_empty() && !manual {
            return None;
        }
        items = global_items(text, start, cursor, index);
    }

    let lower = prefix.to_lowercase();
    let mut recent_rank: HashMap<&str, usize> = HashMap::with_capacity(recent.len());
    for (i, r) in recent.iter().enumerate() {
        recent_rank.entry(r.as_str()).or_insert(i);
    }
    type Key = (u8, usize, bool, Kind, usize);
    let mut scored: Vec<(Key, Item)> = Vec::with_capacity(items.len());
    for it in items.drain(..) {
        // Already fully typed: nothing left to complete (a snippet still expands on Tab).
        if it.label == prefix && it.kind != Kind::Snippet {
            continue;
        }
        let Some(level) = match_level(&it.label, prefix, &lower) else { continue };
        let rank = recent_rank.get(it.label.as_str()).copied().unwrap_or(usize::MAX);
        let key = (level, rank, !it.label.starts_with(prefix), it.kind, it.label.len());
        scored.push((key, it));
    }
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.label.cmp(&b.1.label)));
    let mut seen = HashSet::new();
    let mut items: Vec<Item> = Vec::new();
    for (_, it) in scored {
        if seen.insert((it.label.clone(), it.kind)) {
            items.push(it);
            if items.len() >= MAX_ITEMS {
                break;
            }
        }
    }
    if items.is_empty() { None } else { Some(Completions { start, items }) }
}

/// How many suggestions the popup keeps.
const MAX_ITEMS: usize = 300;

/// 0 = prefix, 1 = camel-case initials, 2 = substring, 3 = scattered letters.
fn match_level(label: &str, prefix: &str, lower: &str) -> Option<u8> {
    if prefix.is_empty() {
        return Some(0);
    }
    let l = label.to_lowercase();
    if l.starts_with(lower) {
        Some(0)
    } else if camel_match(label, prefix) {
        Some(1)
    } else if prefix.len() >= 2 && l.contains(lower) {
        Some(2)
    } else if prefix.len() >= 3 && is_subsequence(lower, &l) {
        Some(3)
    } else {
        None
    }
}

fn is_subsequence(needle: &str, hay: &str) -> bool {
    let mut it = hay.chars();
    needle.chars().all(|n| it.any(|h| h == n))
}

/// `"sb"` matches `StringBuilder`, `"aL"` matches `ArrayList`.
fn camel_match(label: &str, prefix: &str) -> bool {
    if prefix.len() < 2 {
        return false;
    }
    let humps: String = label
        .char_indices()
        .filter(|(i, c)| *i == 0 || c.is_uppercase())
        .map(|(_, c)| c.to_ascii_lowercase())
        .collect();
    humps.starts_with(&prefix.to_lowercase())
}

/// The expression left of the `.` at `dot` (in comment/string-blanked text):
/// `foo`, `System.out`, `names.get(0)`, `new Foo()`, `"x".trim()`, `((A) o)`.
fn receiver_before(clean: &str, dot: usize) -> String {
    let b = clean.as_bytes();
    let mut i = dot.min(b.len());
    while i > 0 && b[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    let end = i;
    for _ in 0..64 {
        // trailing call arguments / index groups, right to left
        while i > 0 && (b[i - 1] == b')' || b[i - 1] == b']') {
            let (open, close) = if b[i - 1] == b')' { (b'(', b')') } else { (b'[', b']') };
            let (mut d, mut j, mut ok) = (0i32, i, false);
            while j > 0 {
                j -= 1;
                if b[j] == close {
                    d += 1;
                } else if b[j] == open {
                    d -= 1;
                    if d == 0 {
                        ok = true;
                        break;
                    }
                }
            }
            if !ok {
                return String::new();
            }
            i = j;
        }
        // generic arguments: `new ArrayList<String>()`
        if i > 0 && b[i - 1] == b'>' {
            let (mut d, mut j, mut ok) = (0i32, i, false);
            while j > 0 {
                j -= 1;
                if b[j] == b'>' {
                    d += 1;
                } else if b[j] == b'<' {
                    d -= 1;
                    if d == 0 {
                        ok = true;
                        break;
                    }
                } else if !(is_ident(b[j]) || b" ,.?[]&".contains(&b[j])) {
                    break;
                }
            }
            if !ok {
                return String::new();
            }
            i = j;
        }
        // a string literal always starts the expression
        if i > 0 && b[i - 1] == b'"' {
            let mut k = i - 1;
            let mut found = false;
            while k > 0 {
                k -= 1;
                if b[k] == b'"' && (k == 0 || b[k - 1] != b'\\') {
                    found = true;
                    break;
                }
            }
            if !found {
                return String::new();
            }
            i = k;
            break;
        }
        let id_end = i;
        while i > 0 && is_ident(b[i - 1]) {
            i -= 1;
        }
        if i == id_end {
            break; // a parenthesised group starts the expression
        }
        // `new Foo(...)`
        let mut k = i;
        while k > 0 && b[k - 1].is_ascii_whitespace() {
            k -= 1;
        }
        if k < i && k >= 3 && &b[k - 3..k] == b"new" && (k == 3 || !is_ident(b[k - 4])) {
            i = k - 3;
            break;
        }
        // chained: `.` before this segment
        if k > 0 && b[k - 1] == b'.' {
            i = k - 1;
            while i > 0 && b[i - 1].is_ascii_whitespace() {
                i -= 1;
            }
            continue;
        }
        break;
    }
    clean.get(i..end).unwrap_or("").trim().to_string()
}

/// Split `a.b(c.d).e[0]` on its top-level dots.
fn split_segments(expr: &str) -> Vec<String> {
    let b = expr.as_bytes();
    let mut segs = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    let mut in_str = false;
    // `new java.util.ArrayList<...>(...)`: the qualified name belongs to the first segment
    let mut in_new_name = expr.trim_start().starts_with("new ");
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                i += 1;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'(' | b'[' => {
                    depth += 1;
                    if c == b'(' {
                        in_new_name = false;
                    }
                }
                b')' | b']' => depth -= 1,
                b'.' if depth == 0 && !in_new_name => {
                    segs.push(expr[start..i].trim().to_string());
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    segs.push(expr[start..].trim().to_string());
    segs
}

fn global_items(text: &str, start: usize, cursor: usize, index: &ProjectIndex) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    for (label, insert, detail) in SNIPPETS {
        items.push(Item::new(label, insert, detail, Kind::Snippet));
    }
    for k in KEYWORDS.iter().chain(PRIMITIVES).chain(["true", "false", "null"].iter()) {
        items.push(Item::new(k, k, "keyword", Kind::Keyword));
    }
    for c in all_lib_class_names() {
        items.push(Item::new(c, c, "java", Kind::Class));
    }
    for c in index.classes.values() {
        items.push(Item::new(&c.name, &c.name, "project", Kind::Class));
    }
    // members of the class the cursor is in, including inherited ones
    if let Some(cls) = enclosing_class(text, cursor) {
        if let Some(members) = members_of_type(&cls, index) {
            items.extend(members);
        }
    }
    // every identifier in the current file
    let word = &text[start..cursor];
    let mut seen: HashSet<&str> = HashSet::new();
    for m in IDENT_RE.find_iter(text) {
        let w = m.as_str();
        if w.len() < 2 || w == word || m.start() == start || KEYWORDS.contains(&w) || PRIMITIVES.contains(&w) {
            continue;
        }
        if seen.insert(w) {
            let kind = if w.as_bytes()[0].is_ascii_uppercase() { Kind::Class } else { Kind::Variable };
            items.push(Item::new(w, w, "", kind));
        }
    }
    items
}

/// The innermost class whose body contains the cursor.
fn enclosing_class(text: &str, cursor: usize) -> Option<String> {
    let clean = blank_comments_and_strings(text);
    let b = clean.as_bytes();
    let mut best: Option<String> = None;
    for c in CLASS_RE.captures_iter(&clean) {
        let m = c.get(0).unwrap();
        if m.start() >= cursor {
            break;
        }
        let Some(open_rel) = clean[m.end()..].find('{') else { continue };
        let open = m.end() + open_rel;
        if open >= cursor {
            continue;
        }
        let mut depth = 0i32;
        let mut end = b.len();
        for (i, &ch) in b.iter().enumerate().skip(open) {
            if ch == b'{' {
                depth += 1;
            } else if ch == b'}' {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
        }
        if cursor <= end {
            best = Some(c[2].to_string());
        }
    }
    best
}

const PRIMITIVE_TYPES: &[&str] = &["int", "long", "short", "byte", "char", "boolean", "float", "double", "void"];

fn is_primitive(ty: &str) -> bool {
    PRIMITIVE_TYPES.contains(&ty.trim())
}

fn base_name(ty: &str) -> &str {
    let b = ty.split('<').next().unwrap_or(ty).trim();
    b.rsplit('.').next().unwrap_or(b)
}

/// Members of `name` and of everything it extends / implements.
fn collect_members(name: &str, index: &ProjectIndex, visited: &mut HashSet<String>, depth: usize, out: &mut Vec<Item>) -> bool {
    if !visited.insert(name.to_string()) {
        return true;
    }
    if let Some(c) = index.classes.get(name) {
        out.extend(c.members.iter().cloned());
        if depth < 8 {
            for s in &c.supers {
                collect_members(s, index, visited, depth + 1, out);
            }
        }
        true
    } else if let Some(m) = lib_members(name) {
        out.extend(m);
        true
    } else {
        false
    }
}

fn members_of_type(ty: &str, index: &ProjectIndex) -> Option<Vec<Item>> {
    let ty = ty.trim();
    if ty.ends_with("[]") {
        return lib_members("array");
    }
    let base = base_name(ty);
    let mut items = Vec::new();
    if !collect_members(base, index, &mut HashSet::new(), 0, &mut items) {
        return None;
    }
    items.extend(lib_members("Object").unwrap_or_default());
    Some(items)
}

fn member_items(receiver: &str, text: &str, cursor: usize, index: &ProjectIndex) -> Vec<Item> {
    if let Some(ty) = resolve_expr(receiver, text, cursor, index) {
        if is_primitive(&ty) {
            return Vec::new();
        }
        if let Some(items) = members_of_type(&ty, index) {
            return items;
        }
    }
    // Unknown receiver: offer everything plausible.
    let mut items: Vec<Item> = index.classes.values().flat_map(|c| c.members.iter().cloned()).collect();
    items.extend(lib_members("Object").unwrap_or_default());
    static AFTER_DOT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.\s*([A-Za-z_$][\w$]*)\s*(\()?").unwrap());
    let mut seen = HashSet::new();
    for c in AFTER_DOT.captures_iter(text) {
        let name = c.get(1).unwrap();
        if name.end() == cursor || !seen.insert(name.as_str().to_string()) {
            continue;
        }
        if c.get(2).is_some() {
            items.push(Item::method(name.as_str(), "…", "?"));
        } else {
            items.push(Item::new(name.as_str(), name.as_str(), "", Kind::Field));
        }
    }
    items
}

// ------------------------------------------------------------ type inference

thread_local! {
    static RESOLVE_DEPTH: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// The static type of a Java expression, as far as this heuristic engine can tell.
fn resolve_expr(expr: &str, text: &str, cursor: usize, index: &ProjectIndex) -> Option<String> {
    let depth = RESOLVE_DEPTH.with(|d| d.get());
    if depth > 5 {
        return None;
    }
    RESOLVE_DEPTH.with(|d| d.set(depth + 1));
    let r = resolve_expr_inner(expr.trim(), text, cursor, index);
    RESOLVE_DEPTH.with(|d| d.set(depth));
    r
}

fn resolve_expr_inner(expr: &str, text: &str, cursor: usize, index: &ProjectIndex) -> Option<String> {
    let segs = split_segments(expr);
    let mut ty = first_type(segs.first()?, text, cursor, index)?;
    for seg in &segs[1..] {
        ty = member_type(&ty, seg, index)?;
    }
    Some(ty)
}

static CAST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\(\s*([A-Za-z_$][\w$.<>,\[\]\s?]*?)\s*\)\s*[\w$("]"#).unwrap());

fn first_type(seg: &str, text: &str, cursor: usize, index: &ProjectIndex) -> Option<String> {
    let seg = seg.trim();
    if seg.starts_with('"') {
        return Some("String".into());
    }
    if let Some(rest) = seg.strip_prefix("new ") {
        let rest = rest.trim_start();
        let end = rest.find(['(', '[']).unwrap_or(rest.len());
        let name = rest[..end].trim();
        if name.is_empty() {
            return None;
        }
        return Some(if rest[end..].starts_with('[') { format!("{name}[]") } else { name.to_string() });
    }
    if seg == "this" {
        return enclosing_class(text, cursor);
    }
    if seg.starts_with('(') && seg.ends_with(')') {
        let inner = seg[1..seg.len() - 1].trim();
        if let Some(c) = CAST_RE.captures(inner) {
            return Some(c[1].trim().to_string());
        }
        return resolve_expr(inner, text, cursor, index);
    }
    let name_end = seg.find(['(', '[']).unwrap_or(seg.len());
    let name = seg[..name_end].trim();
    let tail = &seg[name_end..];
    if name.is_empty() || !name.bytes().all(is_ident) {
        return None;
    }
    let mut ty = if tail.starts_with('(') {
        // a call of a method of the current class (or one it inherits)
        method_ret(&enclosing_class(text, cursor)?, name, index)?
    } else {
        resolve_name(name, text, cursor, index)?
    };
    for _ in after_call(tail).matches('[') {
        ty = ty.strip_suffix("[]")?.to_string();
    }
    Some(ty)
}

/// What follows the first balanced `(...)` of `tail` (or `tail` itself without one).
fn after_call(tail: &str) -> &str {
    if !tail.starts_with('(') {
        return tail;
    }
    let mut d = 0i32;
    for (i, c) in tail.char_indices() {
        match c {
            '(' => d += 1,
            ')' => {
                d -= 1;
                if d == 0 {
                    return &tail[i + 1..];
                }
            }
            _ => {}
        }
    }
    ""
}

fn ret_of(m: &Item) -> String {
    m.detail.rsplit_once(" : ").map(|x| x.1.trim().to_string()).unwrap_or_default()
}

fn method_ret(ty: &str, name: &str, index: &ProjectIndex) -> Option<String> {
    let items = members_of_type(ty, index)?;
    let m = items.iter().find(|m| m.kind == Kind::Method && m.label == name)?;
    let r = ret_of(m);
    (!r.is_empty() && r != "void" && r != "?").then_some(r)
}

/// The type of `.seg` on a value of type `ty`, e.g. `get(0)` on `List<String>` → `String`.
fn member_type(ty: &str, seg: &str, index: &ProjectIndex) -> Option<String> {
    let name_end = seg.find(['(', '[']).unwrap_or(seg.len());
    let name = seg[..name_end].trim();
    let tail = &seg[name_end..];
    let is_call = tail.starts_with('(');
    let items = members_of_type(ty, index)?;
    let m = items
        .iter()
        .find(|m| m.label == name && if is_call { m.kind == Kind::Method } else { m.kind == Kind::Field })?;
    let raw = if is_call { ret_of(m) } else { m.detail.clone() };
    let mut t = substitute(&raw, ty);
    if t.is_empty() || t == "void" || t == "?" || (t.len() == 1 && t.as_bytes()[0].is_ascii_uppercase()) {
        return None;
    }
    for _ in after_call(tail).matches('[') {
        t = t.strip_suffix("[]")?.to_string();
    }
    Some(t)
}

/// `List<Map<String,Integer>>` → `["Map<String,Integer>"]`.
fn type_args(ty: &str) -> Vec<String> {
    let (Some(lt), Some(gt)) = (ty.find('<'), ty.rfind('>')) else { return Vec::new() };
    if gt <= lt {
        return Vec::new();
    }
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    for ch in ty[lt + 1..gt].chars() {
        match ch {
            '<' => {
                depth += 1;
                cur.push(ch);
            }
            '>' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    out.push(cur.trim().to_string());
    out.retain(|s| !s.is_empty() && s != "?");
    out
}

/// Replace the type parameters in a library return type with the receiver's arguments.
fn substitute(ret: &str, recv_ty: &str) -> String {
    let args = type_args(recv_ty);
    let map: Vec<(&str, &str)> = match args.len() {
        1 => vec![("E", args[0].as_str()), ("T", args[0].as_str())],
        2 => vec![("K", args[0].as_str()), ("V", args[1].as_str())],
        _ => return ret.to_string(),
    };
    let mut out = String::new();
    let mut tok = String::new();
    for ch in ret.chars().chain(std::iter::once('\0')) {
        if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            tok.push(ch);
            continue;
        }
        if !tok.is_empty() {
            out.push_str(map.iter().find(|(k, _)| *k == tok).map_or(tok.as_str(), |(_, v)| *v));
            tok.clear();
        }
        if ch != '\0' {
            out.push(ch);
        }
    }
    out
}

/// A variable (declared type, inherited field) or a type name used statically.
fn resolve_name(name: &str, text: &str, cursor: usize, index: &ProjectIndex) -> Option<String> {
    if let Some(t) = declared_type(name, text, cursor, index) {
        return Some(t);
    }
    if let Some(cls) = enclosing_class(text, cursor) {
        if let Some(items) = members_of_type(&cls, index) {
            if let Some(f) = items.iter().find(|m| m.kind == Kind::Field && m.label == name && !m.detail.is_empty()) {
                return Some(f.detail.clone());
            }
        }
    }
    let is_type_name = name.as_bytes()[0].is_ascii_uppercase()
        && (index.classes.contains_key(name) || lib_members(name).is_some());
    is_type_name.then(|| name.to_string())
}

fn declaration_regex(name: &str) -> Option<Regex> {
    thread_local! {
        static CACHE: RefCell<HashMap<String, Regex>> = RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(r) = c.get(name) {
            return Some(r.clone());
        }
        let re = Regex::new(&format!(
            r"([A-Za-z_$][\w$.]*)\s*(<[^;=(){{}}]*>)?\s*((?:\[\s*\])*)\s+{}\s*(?:[=;,):]|$)",
            regex::escape(name)
        ))
        .ok()?;
        if c.len() > 256 {
            c.clear();
        }
        c.insert(name.to_string(), re.clone());
        Some(re)
    })
}

/// Find `Type name` (declaration, parameter, field, pattern) for a variable.
fn declared_type(name: &str, text: &str, cursor: usize, index: &ProjectIndex) -> Option<String> {
    let clean = blank_comments_and_strings(text);
    let re = declaration_regex(name)?;
    let mut best: Option<(usize, String)> = None;
    for c in re.captures_iter(&clean) {
        let ty = &c[1];
        if NOT_TYPES.contains(&ty) || ty == "instanceof" {
            continue;
        }
        let pos = c.get(0).unwrap().start();
        let brackets = c[3].matches('[').count();
        let generics = c.get(2).map_or("", |g| g.as_str());
        let mut ty = if ty == "var" {
            var_type(name, &clean[pos..], text, pos, index).unwrap_or_default()
        } else if let Some(elem) = ty.strip_suffix("...") {
            format!("{elem}{generics}[]")
        } else {
            format!("{ty}{generics}")
        };
        if !ty.is_empty() {
            ty.push_str(&"[]".repeat(brackets));
        }
        if ty.is_empty() {
            continue;
        }
        // Prefer the closest declaration before the cursor.
        let better = match &best {
            None => true,
            Some((bp, _)) => (pos < cursor && (*bp > cursor || pos > *bp)) || (*bp > cursor && pos < *bp),
        };
        if better {
            best = Some((pos, ty));
        }
    }
    best.map(|(_, t)| t)
}

/// The expression at the start of `s`: up to `;` (or an unbalanced `)` of a for header).
fn take_expr(s: &str) -> &str {
    let mut d = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' => d += 1,
            ')' | ']' | '}' => {
                d -= 1;
                if d < 0 {
                    return &s[..i];
                }
            }
            ';' if d == 0 => return &s[..i],
            _ => {}
        }
    }
    s
}

/// `var x = <expr>` and `for (var x : <expr>)`.
fn var_type(name: &str, from: &str, text: &str, pos: usize, index: &ProjectIndex) -> Option<String> {
    let re = Regex::new(&format!(r"^var\s+{}\s*([=:])\s*", regex::escape(name))).ok()?;
    let c = re.captures(from)?;
    let op = c.get(1)?.as_str();
    let init = take_expr(&from[c.get(0)?.end()..]).trim();
    if init.is_empty() {
        return None;
    }
    let ty = resolve_expr(init, text, pos, index)?;
    if op == "=" {
        return Some(ty);
    }
    // for-each: element type of an array or single-parameter collection
    if let Some(e) = ty.strip_suffix("[]") {
        return Some(e.to_string());
    }
    let args = type_args(&ty);
    (args.len() == 1).then(|| args[0].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(text: &str, idx: &ProjectIndex) -> Vec<String> {
        let cur = text.find('|').unwrap();
        let t = text.replacen('|', "", 1);
        complete(&t, cur, idx, true, &[]).map(|c| c.items.into_iter().map(|i| i.label).collect()).unwrap_or_default()
    }

    #[test]
    fn string_members() {
        let idx = ProjectIndex::default();
        let l = labels("void main() { String s = \"hi\"; s.len| }", &idx);
        assert_eq!(l.first().map(String::as_str), Some("length"));
    }

    #[test]
    fn system_out() {
        let idx = ProjectIndex::default();
        let l = labels("class A { void f() { System.out.pr| } }", &idx);
        assert!(l.contains(&"println".to_string()));
    }

    #[test]
    fn generic_list_and_var() {
        let idx = ProjectIndex::default();
        let l = labels("List<String> names = new ArrayList<>(); names.ad|", &idx);
        assert!(l.contains(&"add".to_string()));
        let l = labels("var m = new HashMap<String,Integer>(); m.getO|", &idx);
        assert_eq!(l.first().map(String::as_str), Some("getOrDefault"));
    }

    #[test]
    fn project_class_members() {
        let mut idx = ProjectIndex::default();
        idx.update_file(
            Path::new("Dog.java"),
            "public class Dog {\n    private String name;\n    public void bark(int times) { int local = 1; }\n    public String getName() { return name; }\n}\n",
        );
        let info = &idx.classes["Dog"];
        let names: Vec<_> = info.members.iter().map(|m| m.label.as_str()).collect();
        assert!(names.contains(&"bark") && names.contains(&"getName") && names.contains(&"name"));
        assert!(!names.contains(&"local"));
        let l = labels("Dog d = new Dog(); d.|", &idx);
        assert!(l.contains(&"bark".to_string()));
    }

    #[test]
    fn keywords_and_snippets() {
        let idx = ProjectIndex::default();
        let l = labels("so|", &idx);
        assert!(l.contains(&"sout".to_string()));
        let l = labels("class A { // so| }", &idx);
        assert!(l.is_empty());
    }

    fn labels_recent(text: &str, idx: &ProjectIndex, recent: &[String]) -> Vec<String> {
        let cur = text.find('|').unwrap();
        let t = text.replacen('|', "", 1);
        complete(&t, cur, idx, true, recent).map(|c| c.items.into_iter().map(|i| i.label).collect()).unwrap_or_default()
    }

    #[test]
    fn chained_calls_resolve_types() {
        let idx = ProjectIndex::default();
        let l = labels("List<String> names = new ArrayList<>(); names.get(0).len|", &idx);
        assert_eq!(l.first().map(String::as_str), Some("length"));
        let l = labels("String s = \"a\"; s.trim().toUp|", &idx);
        assert_eq!(l.first().map(String::as_str), Some("toUpperCase"));
        let l = labels("Map<String,Integer> m = new HashMap<>(); m.get(\"k\").intV|", &idx);
        assert!(l.contains(&"intValue".to_string()));
        let l = labels("String[] a = {}; a[0].subs|", &idx);
        assert!(l.contains(&"substring".to_string()));
        let l = labels("new StringBuilder().appe|", &idx);
        assert!(l.contains(&"append".to_string()));
    }

    #[test]
    fn for_each_var_and_entry_set() {
        let idx = ProjectIndex::default();
        let l = labels("Map<String,Integer> m = new HashMap<>(); for (var e : m.entrySet()) { e.getK| }", &idx);
        assert_eq!(l.first().map(String::as_str), Some("getKey"));
        let l = labels("List<String> xs = new ArrayList<>(); for (var x : xs) { x.isBl| }", &idx);
        assert!(l.contains(&"isBlank".to_string()));
        let l = labels("List<String> xs = new ArrayList<>(); var first = xs.get(0); first.trimm|", &idx);
        assert!(l.is_empty());
        let l = labels("List<String> xs = new ArrayList<>(); var first = xs.get(0); first.stri|", &idx);
        assert!(l.contains(&"strip".to_string()));
    }

    #[test]
    fn multi_line_fluent_chain() {
        let idx = ProjectIndex::default();
        let l = labels("List<String> xs = null;\nxs.stream()\n    .filter(x -> x.isEmpty())\n    .coun|", &idx);
        assert_eq!(l.first().map(String::as_str), Some("count"));
    }

    #[test]
    fn inherited_members_and_enums() {
        let mut idx = ProjectIndex::default();
        idx.update_file(
            Path::new("A.java"),
            "class Animal { void eat() {} String name; }\nclass Dog extends Animal { void bark() {} void f() { ea } }\nenum Color { RED, GREEN(1), BLUE { }; int v; Color() {} Color(int x) {} }\nrecord P(int x, int y) {}\n",
        );
        let l = labels("Dog d = new Dog(); d.ea|", &idx);
        assert!(l.contains(&"eat".to_string()));
        let l = labels("Color.R|", &idx);
        assert_eq!(l.first().map(String::as_str), Some("RED"));
        let l = labels("Color.GREEN.ordi|", &idx);
        assert!(l.contains(&"ordinal".to_string()));
        let l = labels("P p = null; p.|", &idx);
        assert!(l.contains(&"x".to_string()) && l.contains(&"toString".to_string()));
        // inside Dog, inherited methods are suggested unqualified
        let src = "class Animal { void eat() {} }\nclass Dog extends Animal { void f() { ea| } }";
        let mut idx2 = ProjectIndex::default();
        idx2.update_file(Path::new("B.java"), &src.replacen('|', "", 1));
        assert!(labels(src, &idx2).contains(&"eat".to_string()));
    }

    #[test]
    fn project_method_chains() {
        let src = "class A {\n  String name() { return \"\"; }\n  A self() { return this; }\n  void f() { self().name().tr| }\n}";
        let mut idx = ProjectIndex::default();
        idx.update_file(Path::new("A.java"), &src.replacen('|', "", 1));
        assert!(labels(src, &idx).contains(&"trim".to_string()));
    }

    #[test]
    fn recently_used_come_first() {
        let idx = ProjectIndex::default();
        let plain = labels_recent("String s = \"\"; s.|", &idx, &[]);
        assert_ne!(plain.first().map(String::as_str), Some("toUpperCase"));
        let boosted = labels_recent("String s = \"\"; s.|", &idx, &["toUpperCase".into(), "trim".into()]);
        assert_eq!(&boosted[..2], ["toUpperCase", "trim"]);
        // recency orders within a match level; it does not beat a better match
        let l = labels_recent("String s = \"\"; s.to|", &idx, &["trim".into(), "toString".into()]);
        assert_eq!(l.first().map(String::as_str), Some("toString"));
    }

    #[test]
    fn opens_from_one_letter_and_hides_exact_matches() {
        let idx = ProjectIndex::default();
        let t = "class A { void f() { Str } }";
        let cur = t.find("Str").unwrap() + 1;
        assert!(complete(t, cur, &idx, false, &[]).is_some());
        // fully typed word: nothing left to complete
        let t = "class A { void f() { String } }";
        let cur = t.find("String").unwrap() + 6;
        let r = complete(t, cur, &idx, false, &[]);
        assert!(r.is_none_or(|c| c.items.iter().all(|i| i.label != "String")));
    }

    #[test]
    fn fuzzy_and_camel_matches() {
        let idx = ProjectIndex::default();
        let l = labels("sb|", &idx);
        assert!(l.contains(&"StringBuilder".to_string()));
        let l = labels("hMap|", &idx);
        assert!(l.contains(&"HashMap".to_string()));
        let l = labels("String s; s.ndex|", &idx);
        assert!(l.contains(&"indexOf".to_string()));
    }
}
