public class Greeter {
    private final String target;

    public Greeter(String target) {
        this.target = target;
    }

    public void greet() {
        System.out.println("Hello, " + target + "!");
    }

    public String getTarget() {
        return target;
    }
}
