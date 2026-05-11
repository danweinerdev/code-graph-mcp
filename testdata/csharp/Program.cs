// Entry point + free function calls + namespace import usage.

using System;
using System.Collections.Generic;
using Models;

namespace App
{
    public static class Program
    {
        public static void Main()
        {
            var svc = new Service();
            svc.Handle();
            Run();
        }

        public static void Run()
        {
            Console.WriteLine("running");
        }
    }
}
