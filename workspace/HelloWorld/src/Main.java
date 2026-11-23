import java.util.*;

/**
 * Press F5 to compile and run. Try Ctrl+Space for code completion.
 */
public class Main {

    record Student(String name, int score) {}

    public static void main(String[] args) {
        List<Student> students = new ArrayList<>();
        students.add(new Student("Ada", 95));
        students.add(new Student("Linus", 88));
        students.add(new Student("Grace", 99));

        students.sort(Comparator.comparingInt(Student::score).reversed());

        System.out.println("Java " + Runtime.version().feature() + " says hello!");
        for (Student s : students) {
            String grade = switch (s.score() / 10) {
                case 10, 9 -> "A";
                case 8 -> "B";
                default -> "C";
            };
            System.out.printf("%-6s %3d  %s%n", s.name(), s.score(), grade);
        }

        Greeter greeter = new Greeter("World");
        greeter.greet();

        Scanner scanner = new Scanner(System.in);
        System.out.print("What's your name? ");
        if (scanner.hasNextLine()) {
            String name = scanner.nextLine();
            System.out.println("Nice to meet you, " + name + "!");
        }
    }
}
