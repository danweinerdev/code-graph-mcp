// Class hierarchy: single inheritance (Beta : Alpha), multiple-base
// (Gamma : Alpha, IMixin), interface implementation (Service : IService),
// generic class (Box<T> : BoxBase<T>) per Decision 9.

using System;

namespace Models
{
    public interface IMixin
    {
        void Mix();
    }

    public interface IService
    {
        void Handle();
    }

    public class Alpha
    {
        public Alpha() { }
        public virtual void M() { Console.WriteLine("alpha"); }
    }

    public class Beta : Alpha
    {
        public Beta() : base() { }
        public override void M() { base.M(); }
    }

    public class Gamma : Alpha, IMixin
    {
        public void Mix() { M(); }
    }

    public class Service : IService
    {
        public void Handle() { }
    }

    public class Box<T> : BoxBase<T>
    {
        public T Value;
    }

    public class BoxBase<T> { }
}
