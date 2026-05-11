// Class hierarchy: single inheritance (Beta extends Alpha), multiple-base
// (Gamma extends Alpha implements IMixin), interface implementation
// (Service implements IService), interface extending interfaces
// (IExtended extends IMixin, IService), generic class
// (Box<T> extends BoxBase<T>) per Decision 9.

package models;

import java.util.List;

public class Models {

    public interface IMixin {
        void mix();
    }

    public interface IService {
        void handle();
    }

    public interface IExtended extends IMixin, IService {
        // Default method, body present — Decision 11 extracts as Function.
        default void doBoth() { mix(); handle(); }
    }

    public static class Alpha {
        public Alpha() { }
        public void m() { System.out.println("alpha"); }
    }

    public static class Beta extends Alpha {
        public Beta() { super(); }
        @Override
        public void m() { super.m(); }
    }

    public static class Gamma extends Alpha implements IMixin {
        @Override
        public void mix() { m(); }
    }

    public static class Service implements IService {
        @Override
        public void handle() { }
    }

    public static class Box<T> extends BoxBase<T> {
        public T value;
    }

    public static class BoxBase<T> { }
}
